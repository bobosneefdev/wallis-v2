import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from piper import PiperVoice, SynthesisConfig

voice = PiperVoice.load(sys.argv[1])

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def do_GET(self):
        self.send_response(200)
        self.end_headers()

    def do_POST(self):
        try:
            body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
            audio = voice.synthesize(body["text"], SynthesisConfig(length_scale=body.get("length_scale")))
            self.send_response(200)
            self.send_header("Content-Type", "audio/L16;rate=22050;channels=1")
            self.end_headers()
            for chunk in audio:
                self.wfile.write(chunk.audio_int16_bytes)
                self.wfile.flush()
        except Exception as error:
            print(error, file=sys.stderr, flush=True)

    def log_message(self, *_):
        pass

HTTPServer(("127.0.0.1", int(sys.argv[2])), Handler).serve_forever()
