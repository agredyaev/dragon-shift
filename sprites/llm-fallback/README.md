# LLM Fallback Sprites

These fallback sprites were generated through the live `POST /api/workshops/sprite-sheet` endpoint.
That endpoint uses the current sprite prompt in `platform/app-server/src/llm.rs`, not a copied prompt.

Current slicing contract:
- top-left: neutral
- top-right: happy
- bottom-left: angry
- bottom-right: sleepy

Each set directory contains:
- `neutral.png`
- `happy.png`
- `angry.png`
- `sleepy.png`
- `meta.json`
