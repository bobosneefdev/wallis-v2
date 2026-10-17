"""CPU-only, loopback speech servers. Models and preset states are local files."""
import io
import json
from pathlib import Path
import socket
import sys
import tempfile
import wave
from http.server import BaseHTTPRequestHandler, HTTPServer


def load(mode, path, threads, voices_dir=None):
    if mode == "stt":
        import numpy as np
        import sherpa_onnx
        model = sherpa_onnx.OfflineRecognizer.from_transducer(
            encoder=str(path / "encoder.int8.onnx"), decoder=str(path / "decoder.int8.onnx"),
            joiner=str(path / "joiner.int8.onnx"), tokens=str(path / "tokens.txt"),
            num_threads=threads, provider="cpu", model_type="nemo_transducer",
            decoding_method="greedy_search")

        def transcribe(body):
            with wave.open(io.BytesIO(body)) as wav:
                if (wav.getframerate(), wav.getnchannels(), wav.getsampwidth(), wav.getcomptype()) != (16000, 1, 2, "NONE"):
                    raise ValueError("Expected 16 kHz mono PCM16 WAV.")
                if not 0 < wav.getnframes() <= 16000 * 120:
                    raise ValueError("Audio must be between 0 and 120 seconds.")
                audio = wav.readframes(wav.getnframes())
                if len(audio) != wav.getnframes() * 2:
                    raise ValueError("Truncated WAV.")
            stream = model.create_stream()
            stream.accept_waveform(16000, np.frombuffer(audio, dtype="<i2").astype(np.float32) / 32768)
            model.decode_stream(stream)
            return json.dumps({"text": stream.result.text}).encode()
        return transcribe

    import yaml
    from pocket_tts import TTSModel
    from pocket_tts.models.model_state import export_model_state
    from pocket_tts.utils.config import CONFIGS_DIR
    pocket = path.parent
    voices = voices_dir  # Cloned voices: <name>.safetensors. Presets live beside the model.

    def build(weights):
        # Reuse the pinned package's architecture, replacing every network source.
        config = yaml.safe_load((CONFIGS_DIR / "english_2026-09.yaml").read_text())
        config["weights_path"] = str(weights)
        config.pop("weights_path_without_voice_cloning", None)
        config["flow_lm"]["lookup_table"]["tokenizer_path"] = str(pocket / "tokenizer.json")
        with tempfile.TemporaryDirectory() as temporary:
            local_config = Path(temporary) / "local.yaml"
            local_config.write_text(yaml.safe_dump(config))
            return TTSModel.load_model(config=local_config)

    model = build(pocket / "model.safetensors")
    model.has_voice_cloning = False
    default_temp, default_eos = model.temp, model.eos_threshold
    cache = {}

    def voice_state(name):
        if not isinstance(name, str) or not name.isascii() or not name.replace("_", "").replace("-", "").isalnum() or len(name) > 40:
            raise ValueError("Invalid voice name.")
        if name not in cache:
            for folder in (voices, pocket):
                if folder and (folder / f"{name}.safetensors").is_file():
                    if len(cache) >= 8:
                        cache.pop(next(iter(cache)))
                    cache[name] = model.get_state_for_audio_prompt(folder / f"{name}.safetensors")
                    break
            else:
                raise ValueError(f"No voice named {name}.")
        return cache[name]

    voice_state(path.stem)
    cloner = []

    def validate_voice(body):
        request = json.loads(body)
        voice_state(request.get("voice") if isinstance(request, dict) else None)
        return b"{}"

    def clone(body):
        request = json.loads(body)
        name, audio = request.get("name"), Path(str(request.get("audio")))
        if not isinstance(name, str) or (pocket / f"{name}.safetensors").is_file():
            raise ValueError("Pick a new voice name that is not a built-in preset.")
        voice_state_check = name.replace("_", "").replace("-", "")
        if not voice_state_check.isalnum() or len(name) > 40 or not audio.is_file() or audio.suffix.lower() != ".wav":
            raise ValueError("Cloning needs a voice name and a 16-bit WAV file.")
        weights = voices.parent / "cloning" / "model.safetensors"
        if not weights.is_file():
            raise ValueError("Voice cloning is not enabled: the cloning weights are not installed.")
        if not cloner:
            cloner.append(build(weights))
        state = cloner[0].get_state_for_audio_prompt(audio, truncate=True)
        voices.mkdir(parents=True, exist_ok=True)
        export_model_state(state, voices / f"{name}.safetensors")
        cache.pop(name, None)
        return json.dumps({"voice": name}).encode()

    def synthesize(body):
        request = json.loads(body)
        text = request.get("text") if isinstance(request, dict) else None
        if not isinstance(text, str) or not text.strip() or len(text) > 16000:
            raise ValueError("Text must contain 1–16000 characters.")
        state = voice_state(request.get("voice") or path.stem)
        # The server handles one request at a time, so per-request sampling is safe to set here.
        model.temp = float(request.get("temp") or default_temp)
        model.eos_threshold = float(request.get("eos_threshold", default_eos))
        if not 0.05 <= model.temp <= 1.5 or not -8.0 <= model.eos_threshold <= 0.0:
            raise ValueError("Sampling settings out of range.")
        return (chunk.detach().cpu().numpy().astype("<f4").tobytes()
                for chunk in model.generate_audio_stream(state, text))
    synthesize.clone = clone
    synthesize.validate_voice = validate_voice
    return synthesize

def serve(mode, path, threads, voices_dir=None):
    inference = load(mode, path.resolve(), threads, voices_dir and voices_dir.resolve())

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def do_GET(self):
            if self.path != "/health":
                return self.send_error(404)
            self.send_response(200)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def do_POST(self):
            if mode == "tts" and self.path == "/clone":
                return self.control(inference.clone, "Voice cloning failed.")
            if mode == "tts" and self.path == "/voice":
                return self.control(inference.validate_voice, "Voice validation failed.")
            if self.path != ("/inference" if mode == "stt" else "/synthesize"):
                return self.send_error(404)
            self.connection.settimeout(30)
            self.connection.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 65536)
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 < length <= (4_000_000 if mode == "stt" else 100_000):
                    raise ValueError("Invalid body size.")
                body = self.rfile.read(length)
                if len(body) != length:
                    raise ValueError("Truncated request.")
                result = inference(body)
                # Generate the first chunk before sending success, so startup failures reach the caller.
                first = next(result) if mode == "tts" else result
            except (ValueError, KeyError, wave.Error) as error:
                return self.send_error(400, "Invalid speech request.", explain=str(error))
            except Exception as error:
                print(error, file=sys.stderr, flush=True)
                return self.send_error(500, "Speech inference failed.")
            self.send_response(200)
            self.send_header("Content-Type", "application/json" if mode == "stt" else "application/octet-stream")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            try:
                def write(chunk):
                    if chunk:
                        self.wfile.write(f"{len(chunk):x}\r\n".encode() + chunk + b"\r\n")
                        self.wfile.flush()
                write(first)
                if mode == "tts":
                    for chunk in result:
                        write(chunk)
                self.wfile.write(b"0\r\n\r\n")
                self.wfile.flush()
            except (ConnectionError, TimeoutError):
                self.close_connection = True
            except Exception as error:
                print(error, file=sys.stderr, flush=True)
                self.close_connection = True  # No terminal chunk: the reader sees a failed stream.

        def control(self, action, failure):
            self.connection.settimeout(300)
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 < length <= 10_000:
                    raise ValueError("Invalid body size.")
                request = self.rfile.read(length)
                if len(request) != length:
                    raise ValueError("Truncated request.")
                body = action(request)
            except (ValueError, KeyError) as error:
                return self.send_error(400, "Invalid voice request.", explain=str(error))
            except Exception as error:
                print(error, file=sys.stderr, flush=True)
                return self.send_error(500, failure)
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    HTTPServer(("127.0.0.1", 8085 if mode == "stt" else 8086), Handler).serve_forever()


if __name__ == "__main__":
    mode, path, threads, *rest = sys.argv[1:]
    if mode not in ("stt", "tts") or not 1 <= int(threads) <= 64:
        raise ValueError("Invalid speech settings.")
    serve(mode, Path(path), int(threads), Path(rest[0]) if rest else None)
