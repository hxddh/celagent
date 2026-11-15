# Bundled assets

Lenslet ships its typography; it never downloads fonts at runtime.

## Fonts

| File | Family | Weight | Source |
| --- | --- | --- | --- |
| `fonts/Inter-Regular.ttf` | Inter | 400 | [Inter 4.1](https://github.com/rsms/inter/releases/tag/v4.1), `extras/ttf/` |
| `fonts/Inter-Medium.ttf` | Inter | 500 | Inter 4.1, `extras/ttf/` |
| `fonts/Inter-SemiBold.ttf` | Inter | 600 | Inter 4.1, `extras/ttf/` |
| `fonts/JetBrainsMono-Regular.ttf` | JetBrains Mono | 400 | [JetBrains Mono 2.304](https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304) |
| `fonts/JetBrainsMono-Medium.ttf` | JetBrains Mono | 500 | JetBrains Mono 2.304 |

Both families are licensed under the SIL Open Font License 1.1:

- `fonts/LICENSE-Inter.txt`
- `fonts/LICENSE-JetBrains-Mono.txt`

The app registers the faces with `cx.text_system().add_fonts(...)` in
`crates/app/src/fonts.rs` and uses the `Inter` and `JetBrains Mono` family
names. If registration fails, it falls back to the platform's generic
`sans-serif` and `monospace` families.

## Icons

The Lenslet aperture mark, search, pause/play, and auth key are painted with
GPUI primitives in `crates/app/src/glyph.rs`; there are no image assets.
