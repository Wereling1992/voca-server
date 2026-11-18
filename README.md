# voca-server

Extremely fast **English** Text-to-Speech server powered by [sultson/voca](https://github.com/sultson/voca) (Rust rewrite of [tiny-voca](https://github.com/n4ze3m/tiny-voca)).

OpenAI-compatible API · **CPU-only** · ~20 MB model · single binary

**Docker Hub:** [wereling1992/voca-server](https://hub.docker.com/r/wereling1992/voca-server)

## Features

- OpenAI-compatible API (`POST /v1/audio/speech`)
- **CPU-only** — no GPU required
- Extremely fast (Rust + tiny model)
- Model baked into the image (~19 MB) → no volume required
- Two fixed voices (F2 female / M2 male)
- Fully offline after pull
- Optional API key authentication

## Quick Start

### Docker (recommended)

```bash
docker run -d \
  --name voca-server \
  -p 3420:3420 \
  -e API_KEY=MySuperSafeKey \
  -e REQUIRE_API_KEY=1 \
  --restart unless-stopped \
  wereling1992/voca-server:latest
```

Model is baked into the image (~19 MB). No volume required.

#### Health check

```bash
curl http://localhost:3420/health
```

#### Synthesize

```bash
curl -X POST http://localhost:3420/v1/audio/speech \
  -H "Content-Type: application/json" \
  -H "x-api-key: MySuperSafeKey" \
  -d '{
    "input": "Hello! This is tiny voca running in Rust.",
    "voice": "F2",
    "speed": 1.0,
    "response_format": "wav"
  }' \
  --output out.wav
```

## Environment Variables

| Variable        | Default | Description                |
|-----------------|---------|----------------------------|
| PORT            | 3420    | Listen port                |
| API_KEY         | (empty) | Shared secret              |
| REQUIRE_API_KEY | 0       | Set 1 to enforce auth      |
| DEFAULT_VOICE   | F2      | Fallback voice             |

## API

### `POST /v1/audio/speech`

| Field           | Default | Description              |
|-----------------|---------|--------------------------|
| input           | –       | Text to speak (required) |
| voice           | F2      | F2 (female) or M2 (male) |
| speed           | 1.0     | 0.25 – 4.0               |
| pitch           | 0       | Pitch shift in semitones |
| pitch_range     | 1.0     | 0 – 3                    |
| response_format | wav     | wav or pcm               |

Auth (when `REQUIRE_API_KEY=1`): `Authorization: Bearer <key>` or `x-api-key: <key>`

### Other endpoints

| Endpoint       | Description             |
|----------------|-------------------------|
| GET /health    | Health check            |
| GET /v1/models | List models             |

## Notes

- English only · two fixed voices · no voice cloning
- Fully offline after pull

## License

Apache-2.0 (same as upstream)

## Credits

- [sultson/voca](https://github.com/sultson/voca) – Rust rewrite
- [n4ze3m/tiny-voca](https://github.com/n4ze3m/tiny-voca) – original
