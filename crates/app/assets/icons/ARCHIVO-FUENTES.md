# Fuentes de los logos (uso personal)
Simple Icons (CC0; las marcas son de sus dueños). Descargados una vez con curl desde `https://cdn.jsdelivr.net/npm/simple-icons@<versión>/icons/<slug>.svg`.

| Agente | Archivo | Versión | Slug / nota |
|---|---|---|---|
| Claude | claude.svg | 16.38.1 | claude |
| Codex | openai.svg | 15.0.0 | openai (ya no existe en 16.38.1) |
| Gemini | googlegemini.svg | 16.38.1 | googlegemini (sin ícono de Antigravity) |
| Grok | grok.svg | propio | xai/grok no existen en Simple Icons; anillo con barra dibujado a mano |
| Junie | jetbrains.svg | 16.38.1 | jetbrains (Junie no existe; sustituto) |
| opencode | opencode.svg | 16.38.1 | opencode |

Los `.a8` son la máscara alfa 64x64 (8 bits) generada una vez con ImageMagick 7.1.2:
`magick -background none -density 1536 X.svg -resize 64x64 -gravity center -extent 64x64 -channel A -separate -depth 8 gray:X.a8`
Se embeben con `include_bytes!` y se tiñen en runtime (sin red, sin crates nuevos).
