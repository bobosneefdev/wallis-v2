"""Runnable real-model check: <app_data_dir>, using the provisioned Python environment."""
import http.client
import json
from pathlib import Path
import subprocess
import sys
import time
import urllib.request
import wave
import io

import torch
assert torch.version.cuda is None, "Expected CPU-only PyTorch"

root = Path(sys.argv[1]).resolve()
script = root / "tools/speech_server.py"
model = root / "models/parakeet-0.6b-v3-int8/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
children = [subprocess.Popen([sys.executable, str(script), "stt", str(model), "4"]),
            subprocess.Popen([sys.executable, str(script), "tts", str(root / "models/neutts-2e"), "4"])]


def request(port, path, body):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=60)
    connection.request("POST", path, body)
    return connection, connection.getresponse()


try:
    for port in (8085, 8086):
        deadline = time.monotonic() + (60 if port == 8085 else 180)
        while True:
            assert all(child.poll() is None for child in children), "Speech server exited"
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=1).close()
                break
            except OSError:
                assert time.monotonic() < deadline, "Speech server did not start"
                time.sleep(0.2)
    for port, path, body in [(8085, "/inference", b"not a WAV"), (8086, "/synthesize", b'{"text":""}'), (8086, "/synthesize", b"[]")]:
        connection, response = request(port, path, body)
        assert response.status == 400, response.status
        response.read()
        connection.close()
    # Unknown speakers, emotions and temperatures are rejected, and every speaker and emotion still speaks afterwards.
    for body in (b'{"text":"Hi.","voice":"missing"}', b'{"text":"Hi.","emotion":"bored"}', b'{"text":"Hi.","temp":9}', b'{"text":"Hi.","voice":"../emily"}'):
        connection, response = request(8086, "/synthesize", body)
        assert response.status == 400, (body, response.status)
        response.read()
        connection.close()
    for voice, emotion in (("emily", "neutral"), ("paul", "angry"), ("sophie", "happy"), ("steven", "sad"), ("emily", "surprised")):
        connection, response = request(8086, "/synthesize", json.dumps({"text": "I can still speak.", "voice": voice, "emotion": emotion}))
        assert response.status == 200 and len(response.read()) > 24000, (voice, emotion)
        connection.close()
    start = time.monotonic()
    connection, response = request(8086, "/synthesize", json.dumps({"text": "Hello, this is a test of the Wallis voice."}))
    assert response.status == 200 and response.getheader("Transfer-Encoding") == "chunked"
    first = response.read(4)
    first_ms = (time.monotonic() - start) * 1000
    assert len(first) == 4
    audio = first + response.read()
    connection.close()
    import numpy as np
    samples = np.frombuffer(audio, dtype="<f4")
    assert len(samples) > 24000 and np.isfinite(samples).all()
    # 24 kHz to 16 kHz for Parakeet; this also checks the stream's declared format.
    pcm = (np.interp(np.arange(0, len(samples), 1.5), np.arange(len(samples)), samples).clip(-1, 1) * 32767).astype("<i2")
    wav = io.BytesIO()
    with wave.open(wav, "wb") as out:
        out.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
        out.writeframes(pcm.tobytes())
    connection, response = request(8085, "/inference", wav.getvalue())
    assert response.status == 200
    text = json.loads(response.read())["text"]
    connection.close()
    assert "test" in text.lower(), text
    # Close a long response after its first sample, then verify the server serves a new request.
    connection, response = request(8086, "/synthesize", json.dumps({"text": "This is a long interruption test. " * 100}))
    assert response.status == 200 and len(response.read(4)) == 4
    response.close()
    connection.close()
    connection, response = request(8086, "/synthesize", json.dumps({"text": "We can speak again."}))
    assert response.status == 200 and len(response.read()) > 24000
    connection.close()
    print(f"PASS: first sample {first_ms:.0f} ms; {len(samples) / 24000:.2f} s audio; transcript {text!r}; cancellation, speaker and emotion switching, invalid settings recovery")
finally:
    for child in children:
        child.terminate()
    for child in children:
        child.wait(timeout=10)
