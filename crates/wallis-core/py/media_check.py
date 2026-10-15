"""Run with the provisioned Python: python media_check.py. No network or Discord account required."""
import base64
import io
import json
import socket
import subprocess
import sys
from pathlib import Path
from unittest.mock import patch

import av
import media


def fixture(fmt, codec, frames=1):
    output = io.BytesIO()
    with av.open(output, mode="w", format=fmt) as container:
        stream = container.add_stream(codec, rate=10)
        stream.width = stream.height = 32
        stream.pix_fmt = {"png": "rgb24", "mjpeg": "yuvj420p", "gif": "rgb8"}.get(codec, "yuv420p")
        for index in range(frames):
            frame = av.VideoFrame(32, 32, "rgb24")
            frame.planes[0].update(bytes([index * 20]) * frame.planes[0].buffer_size)
            for packet in stream.encode(frame):
                container.mux(packet)
        for packet in stream.encode():
            container.mux(packet)
    return output.getvalue()


def rejected(call, text):
    try:
        call()
    except media.MediaError as error:
        assert text in str(error), str(error)
    else:
        raise AssertionError("Unsafe media was accepted")


for fmt, codec, count in [("image2pipe", "png", 1), ("image2pipe", "mjpeg", 1),
                          ("gif", "gif", 12), ("mp4", "mpeg4", 12)]:
    result = media.decode(fixture(fmt, codec, count), 6)
    assert 1 <= len(result["images"]) <= 6
    assert (len(result["images"]) > 1) == (count > 1)
    assert len({image["data"] for image in result["images"]}) == len(result["images"])
    for image in result["images"]:
        assert image["mimeType"] == "image/jpeg"
        with av.open(io.BytesIO(base64.b64decode(image["data"]))) as decoded:
            frame = next(decoded.decode(video=0))
            assert 0 < frame.width <= 768 and 0 < frame.height <= 768
    assert "no audio" in result["note"]

with patch.object(media, "MAX_PIXELS", 100):
    rejected(lambda: media.decode(fixture("image2pipe", "png"), 6), "16 megapixel")

# DNS is checked on every connection, including redirects and mixed public/private answers.
for address in ["127.0.0.1", "10.0.0.1", "169.254.169.254", "::1", "::ffff:127.0.0.1", "224.0.0.1", "fec0::1"]:
    with patch.object(socket, "getaddrinfo", return_value=[(2, 1, 6, "", (address, 443))]):
        rejected(lambda: media.public_addresses("example.com"), "non-public")
with patch.object(socket, "getaddrinfo", return_value=[(2, 1, 6, "", ("8.8.8.8", 443)), (2, 1, 6, "", ("127.0.0.1", 443))]):
    rejected(lambda: media.public_addresses("example.com"), "non-public")
for url in ["file:///etc/passwd", "http://example.com/a.png", "https://user:secret@example.com/a.png", "https://example.com:444/a.png"]:
    rejected(lambda: media.download(url), "public HTTPS")


class Response:
    status = 200
    headers = {}

    def getheader(self, name, default=None):
        return self.headers.get(name, default)

    body = b"image"

    def read(self, count):
        return self.body[:count]


class Connection:
    responses = []

    def __init__(self, *args, **kwargs):
        pass

    def request(self, *args, **kwargs):
        pass

    def getresponse(self):
        return self.responses.pop(0)

    def close(self):
        pass


with patch.object(media, "PublicHTTPS", Connection):
    response = Response()
    response.headers = {"Content-Length": str(media.MAX_BYTES + 1)}
    Connection.responses = [response]
    rejected(lambda: media.download("https://example.com/a.png"), "20 MiB")
    response.headers = {}
    response.body = bytes(media.MAX_BYTES + 1)
    Connection.responses = [response]
    rejected(lambda: media.download("https://example.com/a.png"), "20 MiB")
    response.body = b"image"
    response.headers = {"Content-Type": "text/html"}
    Connection.responses = [response]
    rejected(lambda: media.download("https://example.com/page"), "web page")
    response.headers = {}
    response.status = 403
    Connection.responses = [response]
    rejected(lambda: media.download("https://example.com/a.png"), "HTTP 403")
    response.status = 302
    response.headers = {"Location": "http://127.0.0.1/private"}
    Connection.responses = [response]
    rejected(lambda: media.download("https://example.com/a.png"), "public HTTPS")
    response.headers = {"Location": "https://example.com/loop"}
    Connection.responses = [response] * 4
    rejected(lambda: media.download("https://example.com/a.png"), "too many redirects")

# Run the actual process boundary too: rejected URLs produce structured, secret-free failure notes.
result = subprocess.run([sys.executable, "-I", str(Path(media.__file__))],
                        input=json.dumps({"url": "https://secret@example.com/a.png", "limit": 6}),
                        text=True, capture_output=True, timeout=25, check=True)
result = json.loads(result.stdout)
assert not result["images"] and "secret" not in result["note"]
print("Media checks passed: image/GIF/MP4 bytes, frame limits, HTTPS/DNS/redirect guards, size and error handling.")
