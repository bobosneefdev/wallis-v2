"""Loopback speech servers: Parakeet speech recognition and NeuTTS-2E speech. Models are local files."""
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

    return load_tts(path, threads)

SPEAKERS = ("emily", "paul", "sophie", "steven")
EMOTIONS = ("angry", "disgusted", "fearful", "happy", "neutral", "sad", "surprised")
LLAMA_PORT, HOP, CHUNK, LOOKAHEAD, LOOKBACK, OVERLAP = 8087, 480, 25, 5, 50, 1

def load_tts(path, threads):
    """NeuTTS-2E: a GGUF backbone served by a local llama-server (Vulkan GPU when there is one, else CPU), decoded by NeuCodec
    and watermarked with Perth, as the model's licence requires. `path` holds the GGUF, NeuCodec's weights and the speakers' reference codes."""
    import ctypes
    import os
    import re
    import signal
    import subprocess
    import time
    import unicodedata
    import urllib.request
    import numpy as np
    import requests
    import torch
    import perth
    from neucodec.codec_decoder_vocos import CodecDecoderVocos

    torch.set_num_threads(max(1, threads // 2))
    refs = json.loads((path / "speakers.json").read_text())
    server = subprocess.Popen(
        [str(path.parent.parent / "tools/llama/llama-server"), "-m", str(path / "neutts-2e-Q4_0.gguf"), "-ngl", "99", "-t", str(threads), "-c", "2048",
         "--port", str(LLAMA_PORT), "--host", "127.0.0.1", "--no-webui", "-np", "1"],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=sys.stderr,
        preexec_fn=lambda: ctypes.CDLL("libc.so.6").prctl(1, signal.SIGKILL))  # The backbone dies with this server, however this one ends.
    deadline = time.monotonic() + 120
    while True:
        if server.poll() is not None:
            raise RuntimeError("llama-server exited while loading the voice model.")
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{LLAMA_PORT}/health", timeout=1).close()
            break
        except OSError:
            if time.monotonic() > deadline:
                raise RuntimeError("llama-server did not start.")
            time.sleep(0.2)

    class Decoder(torch.nn.Module):
        def __init__(self):
            super().__init__()
            self.generator = CodecDecoderVocos(hop_length=HOP)
            self.fc_post_a = torch.nn.Linear(2048, 1024)

        def forward(self, codes):
            emb = self.generator.quantizer.get_output_from_indices(codes.transpose(1, 2)).transpose(1, 2)
            return self.generator(self.fc_post_a(emb.transpose(1, 2)), vq=False)[0]

    decoder = Decoder().eval()
    weights = torch.load(path / "neucodec.bin", map_location="cpu", weights_only=True)
    decoder.load_state_dict({k: v for k, v in weights.items() if k.startswith(("generator.", "fc_post_a."))}, strict=False)
    del weights
    watermark = perth.PerthImplicitWatermarker().apply_watermark

    def decode(ids):
        with torch.no_grad():
            wav = decoder(torch.tensor(ids, dtype=torch.long)[None, None, :])[0, 0].numpy()
        return watermark(wav, sample_rate=24_000)

    def overlap_add(frames, stride):
        total = max(stride * i + len(f) for i, f in enumerate(frames))
        out, weight = np.zeros(total, np.float32), np.zeros(total, np.float32)
        for i, frame in enumerate(frames):
            w = (0.5 - np.abs(np.linspace(0, 1, len(frame) + 2, dtype=np.float32)[1:-1] - 0.5))
            out[stride * i:stride * i + len(frame)] += w * frame
            weight[stride * i:stride * i + len(frame)] += w
        return out / weight

    def tokens(prompt, temp, limit):
        body = {"prompt": prompt, "n_predict": limit, "temperature": temp, "top_k": 50, "seed": int.from_bytes(os.urandom(4), "little"),
                "stop": ["<|SPEECH_GENERATION_END|>"], "stream": True, "cache_prompt": True}
        with requests.post(f"http://127.0.0.1:{LLAMA_PORT}/completion", json=body, stream=True, timeout=60) as response:
            response.raise_for_status()
            for line in response.iter_lines():
                if line.startswith(b"data: "):
                    found = re.findall(r"<\|speech_(\d+)\|>", json.loads(line[6:]).get("content", ""))
                    yield from map(int, found)

    def normalize(text):
        return unicodedata.normalize("NFKC", text.translate(str.maketrans({"\u2018": "'", "\u2019": "'", "\u201c": '"', "\u201d": '"'})))

    def stream(text, speaker, emotion, temp):
        ref = refs[speaker]
        mark = "" if emotion == "neutral" else f"<|{emotion.upper()}|>"
        prompt = (f"<|TEXT_PROMPT_START|>{normalize(ref['text'])}{mark}{normalize(text)}<|TEXT_PROMPT_END|>"
                  f"<|SPEECH_GENERATION_START|>{''.join(f'<|speech_{i}|>' for i in ref['codes'])}")
        cache, frames, done_samples, done_tokens, stride = list(ref["codes"]), [], 0, len(ref["codes"]), CHUNK * HOP
        # Speech runs about 15 characters a second; allow generously more than that so a model that fails to stop cannot ramble.
        for token in tokens(prompt, temp, min(1200, 50 * (2 + len(text) // 6))):
            cache.append(token)
            if len(cache) - done_tokens >= CHUNK + LOOKAHEAD:
                start = max(done_tokens - LOOKBACK - OVERLAP, 0)
                wav = decode(cache[start:done_tokens + CHUNK + LOOKAHEAD + OVERLAP])
                first = (done_tokens - start) * HOP
                frames.append(wav[first:first + (CHUNK + 2 * OVERLAP) * HOP])
                merged = overlap_add(frames, stride)[done_samples:len(frames) * stride]
                done_samples, done_tokens = len(frames) * stride, done_tokens + CHUNK
                yield merged.clip(-1, 1).astype("<f4").tobytes()
        if len(cache) > done_tokens:
            left = len(cache) - done_tokens
            start = max(len(cache) - (LOOKBACK + OVERLAP + left), 0)
            wav = decode(cache[start:])[(len(cache) - start - left - OVERLAP) * HOP:]
            frames.append(wav)
            yield overlap_add(frames, stride)[done_samples:].clip(-1, 1).astype("<f4").tobytes()

    def synthesize(body):
        request = json.loads(body)
        if not isinstance(request, dict):
            raise ValueError("Expected a JSON object.")
        text, speaker, emotion, temp = request.get("text"), request.get("voice", "emily"), request.get("emotion", "neutral"), request.get("temp", 1.0)
        if not isinstance(text, str) or not text.strip() or len(text) > 2000:
            raise ValueError("Text must contain 1\u20132000 characters.")
        if speaker not in SPEAKERS or emotion not in EMOTIONS:
            raise ValueError(f"Voice is one of {', '.join(SPEAKERS)}; emotion is one of {', '.join(EMOTIONS)}.")
        if not isinstance(temp, (int, float)) or not 0.3 <= temp <= 1.5:
            raise ValueError("Sampling settings out of range.")
        return stream(text, speaker, emotion, float(temp))

    for _ in synthesize(json.dumps({"text": "Warming up."}).encode()):
        pass
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
