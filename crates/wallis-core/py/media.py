"""Bounded HTTPS media fetch and frame extraction. JSON on stdin/stdout; no disk cache."""
import base64
import http.client
import io
import ipaddress
import json
import socket
import ssl
import sys
from urllib.parse import urljoin, urlsplit

MAX_BYTES = 20 * 1024 * 1024
MAX_PIXELS = 16_000_000


def public_addresses(host):
    addresses = socket.getaddrinfo(host, 443, type=socket.SOCK_STREAM)
    ips = [ipaddress.ip_address(item[4][0]) for item in addresses]
    if not ips or any(not ip.is_global or ip.is_multicast or ip.is_reserved or getattr(ip, "is_site_local", False) for ip in ips):
        raise MediaError("Media URL resolves to a non-public address.")
    return addresses


class MediaError(ValueError):
    pass


class PublicHTTPS(http.client.HTTPSConnection):
    def connect(self):
        # Connect to the exact validated address; a second DNS lookup allows rebinding.
        addresses = public_addresses(self.host)
        last = None
        for family, kind, protocol, _, address in addresses:
            sock = socket.socket(family, kind, protocol)
            sock.settimeout(self.timeout)
            try:
                sock.connect(address)
                self.sock = self._context.wrap_socket(sock, server_hostname=self.host)
                return
            except OSError as error:
                sock.close()
                last = error
        raise OSError("Could not connect to the media host.") from last


def download(url):
    for _ in range(4):
        parsed = urlsplit(url)
        if (parsed.scheme != "https" or not parsed.hostname or parsed.username is not None
                or parsed.password is not None or parsed.port not in (None, 443)):
            raise MediaError("Media requires a public HTTPS URL on port 443 without credentials.")
        connection = PublicHTTPS(parsed.hostname, timeout=5, context=ssl.create_default_context())
        try:
            connection.request("GET", (parsed.path or "/") + ("?" + parsed.query if parsed.query else ""),
                               headers={"User-Agent": "Wallis/0.3", "Accept-Encoding": "identity"})
            response = connection.getresponse()
            if response.status in (301, 302, 303, 307, 308):
                location = response.getheader("Location")
                if not location:
                    raise MediaError("Media redirect has no destination.")
                url = urljoin(url, location)
                continue
            if response.status != 200:
                raise MediaError(f"Media download returned HTTP {response.status} (the link may have expired).")
            if int(response.getheader("Content-Length", "0")) > MAX_BYTES:
                raise MediaError("Media exceeds the 20 MiB download limit.")
            if "text/html" in response.getheader("Content-Type", "").lower():
                raise MediaError("The link is a web page; Discord did not provide a playable media embed.")
            data = response.read(MAX_BYTES + 1)
            if len(data) > MAX_BYTES:
                raise MediaError("Media exceeds the 20 MiB download limit.")
            return data
        finally:
            connection.close()
    raise MediaError("Media has too many redirects.")


def decode(data, limit):
    import av

    images, timestamps = [], []
    # Bytes only: decoders never receive URLs, playlists, or filesystem paths.
    with av.open(io.BytesIO(data), options={"protocol_whitelist": "", "max_pixels": str(MAX_PIXELS)}) as container:
        stream = next((s for s in container.streams.video), None)
        if stream is None:
            raise MediaError("The attachment contains no decodable image or video.")
        if stream.width * stream.height > MAX_PIXELS:
            raise MediaError("Media exceeds the 16 megapixel limit.")
        # ponytail: sampled frames from 30 seconds, no audio; use native video when Pi supports it.
        duration = float(container.duration or 0) / av.time_base
        end = min(duration, 30) if duration > 0 else 1
        interval = max(end / max(limit - 1, 1), 0.1)
        for index, frame in enumerate(container.decode(stream)):
            if frame.width * frame.height > MAX_PIXELS:
                raise MediaError("Media exceeds the 16 megapixel limit.")
            at = max(0, float(frame.time or 0) - float(stream.start_time or 0) * stream.time_base)
            if index >= 1200 or at > 30:
                break
            if images and at + 0.001 < len(images) * interval:
                continue
            scale = min(1, 768 / max(frame.width, frame.height))
            frame = frame.reformat(width=max(2, int(frame.width * scale) // 2 * 2),
                                   height=max(2, int(frame.height * scale) // 2 * 2), format="yuvj420p")
            output = io.BytesIO()
            with av.open(output, mode="w", format="image2pipe") as encoder:
                target = encoder.add_stream("mjpeg", rate=1)
                target.width, target.height, target.pix_fmt = frame.width, frame.height, "yuvj420p"
                frame.pts = None
                for packet in target.encode(frame):
                    encoder.mux(packet)
                for packet in target.encode():
                    encoder.mux(packet)
            images.append({"type": "image", "mimeType": "image/jpeg",
                           "data": base64.b64encode(output.getvalue()).decode("ascii")})
            timestamps.append(round(at, 3))
            if len(images) >= limit:
                break
    if not images:
        raise MediaError("The media could not be decoded.")
    return {"images": images, "note": f"Sampled frames at {timestamps} seconds; at most the first 30 seconds, no audio. Describe only what these frames show."}


def main():
    try:
        request = json.load(sys.stdin)
        result = decode(download(request["url"]), max(1, min(int(request["limit"]), 6)))
    except Exception as error:
        # Do not echo signed URLs, response bodies, or decoder diagnostics into chat/logs.
        message = str(error) if isinstance(error, MediaError) else "Media download or decoding failed."
        result = {"images": [], "note": message}
    print(json.dumps(result))


if __name__ == "__main__":
    main()
