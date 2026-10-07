"""Offline speech-to-text service used by Coucou."""

from pathlib import Path
import tempfile

import av
import numpy as np
from flask import Flask, jsonify, request
from faster_whisper import WhisperModel

MODEL_NAME = "base.en"
TRANSCRIPTION_LANGUAGE = "en"
MIN_CONFIDENCE = -8.0
DEVICE = "cpu"
COMPUTE_TYPE = "int8"
BEAM_SIZE = 1
VAD_FILTER = True
SAMPLE_RATE = 16_000
HOST = "127.0.0.1"
PORT = 5005
MAX_AUDIO_BYTES = 25 * 1024 * 1024

app = Flask(__name__)
app.config["MAX_CONTENT_LENGTH"] = MAX_AUDIO_BYTES
model = WhisperModel(MODEL_NAME, device=DEVICE, compute_type=COMPUTE_TYPE)

def decode_audio(path):
    resampler = av.audio.resampler.AudioResampler(
        format="s16",
        layout="mono",
        rate=SAMPLE_RATE,
    )
    samples = []
    with av.open(str(path), mode="r") as container:
        for frame in container.decode(audio=0):
            samples.extend(resampler.resample(frame))
        samples.extend(resampler.resample(None))
    if not samples:
        return np.array([], dtype=np.float32)
    pcm = np.concatenate([frame.to_ndarray().reshape(-1) for frame in samples])
    return pcm.astype(np.float32) / 32768.0


@app.after_request
def add_cors_headers(response):
    response.headers["Access-Control-Allow-Origin"] = "*"
    response.headers["Access-Control-Allow-Headers"] = "Content-Type"
    response.headers["Access-Control-Allow-Methods"] = "GET, POST, OPTIONS"
    return response


@app.get("/health")
def health():
    return jsonify({"status": "ok"})


@app.route("/transcribe", methods=["POST", "OPTIONS"])
def transcribe():
    if request.method == "OPTIONS":
        return ("", 204)
    audio = request.files.get("audio")
    if audio is None or not audio.filename:
        return jsonify({"error": "An audio file is required."}), 400

    temp_path = None
    try:
        with tempfile.NamedTemporaryFile(suffix=Path(audio.filename).suffix or ".webm", delete=False) as temp:
            temp_path = Path(temp.name)
            audio.save(temp)
        segments, _ = model.transcribe(
            decode_audio(temp_path),
            beam_size=BEAM_SIZE,
            temperature=0,
            vad_filter=VAD_FILTER,
            task="transcribe",
            language=TRANSCRIPTION_LANGUAGE,
            condition_on_previous_text=False,
            without_timestamps=True,
        )
        segments = list(segments)
        text = " ".join(segment.text.strip() for segment in segments).strip()
        if not text:
            return jsonify({"text": "", "confidence": -100.0})
        average_logprob = sum(segment.avg_logprob for segment in segments) / len(segments)
        confidence = max(MIN_CONFIDENCE, min(0.0, average_logprob))
        return jsonify({"text": text, "confidence": round(confidence, 3)})
    except Exception:
        app.logger.exception("Audio transcription failed")
        return jsonify({"error": "Audio transcription failed."}), 500
    finally:
        if temp_path is not None:
            temp_path.unlink(missing_ok=True)


if __name__ == "__main__":
    app.run(host=HOST, port=PORT, debug=False, threaded=True)
