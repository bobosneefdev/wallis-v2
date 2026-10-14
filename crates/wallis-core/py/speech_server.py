"""CPU-only, loopback speech servers. Models and preset states are local files."""
import io
import json
from pathlib import Path
import socket
import sys
import wave
from http.server import BaseHTTPRequestHandler, HTTPServer


def load(mode, path, threads):
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
    from pocket_tts.utils.config import CONFIGS_DIR
    # Reuse the pinned package's architecture, replacing every network source.
    config = yaml.safe_load((CONFIGS_DIR / "english_2026-09.yaml").read_text())
    config["weights_path"] = str(path.parent / "model.safetensors")
    config.pop("weights_path_without_voice_cloning", None)
    config["flow_lm"]["lookup_table"]["tokenizer_path"] = str(path.parent / "tokenizer.json")
    local_config = path.parent / "local.yaml"
    local_config.write_text(yaml.safe_dump(config))
    model = TTSModel.load_model(config=local_config)
    model.has_voice_cloning = False
    state = model.get_state_for_audio_prompt(path)

    def synthesize(body):
        request = json.loads(body)
        text = request.get("text") if isinstance(request, dict) else None
        if not isinstance(text, str) or not text.strip() or len(text) > 16000:
            raise ValueError("Text must contain 1–16000 characters.")
        return (chunk.detach().cpu().numpy().astype("<f4").tobytes()
                for chunk in model.generate_audio_stream(state, text))
    return synthesize


def serve(mode, path, threads):
    inference = load(mode, path.resolve(), threads)

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def do_GET(self):
            if self.path != "/health":
                return self.send_error(404)
            self.send_response(200)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def do_POST(self):
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

        def log_message(self, *_):
            pass

    HTTPServer(("127.0.0.1", 8085 if mode == "stt" else 8086), Handler).serve_forever()


if __name__ == "__main__":
    mode, path, threads = sys.argv[1:]
    if mode not in ("stt", "tts") or not 1 <= int(threads) <= 64:
        raise ValueError("Invalid speech settings.")
    serve(mode, Path(path), int(threads))
