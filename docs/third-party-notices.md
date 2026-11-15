# Third-Party Notices

This project includes components and CSS derived from a third-party open source
project. Attribution, the licensing status that could be verified, and the
licence text used for that attribution are recorded below.

## animated-characters-login-page

- Source: https://github.com/guohaolian/animated-characters-login-page
- Author: guohaolian (GitHub account)
- Upstream revision inspected: `13d2770` (committed 2026-05-31)
- Licence this project attributes the adapted work under: MIT (text transcribed
  below)

### Derived files

| File in this repository | Derived from |
| --- | --- |
| `frontend/src/components/auth/AnimatedCharacters.vue` | `src/components/AnimatedCharacters.vue` |
| `frontend/src/components/auth/EyeBall.vue` | `src/components/EyeBall.vue` |
| `frontend/src/components/auth/Pupil.vue` | `src/components/Pupil.vue` |
| `frontend/src/style.css` (the `auth-login*`, `auth-characters*`, `auth-character*`, `auth-eyeball` and `auth-pupil` rule blocks only) | component styles from the files above plus `src/components/LoginPage.vue` |

### What was adapted

- Character geometry and motion: the four characters, eye/pupil tracking,
  blinking, mouth states (idle/typing/sad), eye-to-eye look and the entrance
  animations.
- Sign-in card visual reference: centred header, 44px inputs with 9px radius,
  and the primary button hover motion (lift, shadow, label and arrow shifting).
- The character and mouth CSS in `frontend/src/style.css`.

### What was changed during adaptation

- Components were rewritten in TypeScript with explicit props.
- Pointer tracking moved out of every eye/pupil into one `requestAnimationFrame`
  throttled, passive `pointermove` listener on the container.
- `prefers-reduced-motion: reduce` is honoured (no pointer tracking, blinking,
  peeking or confetti-style animation).
- The sign-in card uses this project's own CSS custom properties, markup and
  translations instead of the upstream colours and copy.
- No upstream brand name, logo or image asset is reused.

### Licensing status (checked when the code was adapted)

- The upstream repository contains **no** `LICENSE` file at the revision listed
  above (checked against the repository file list).
- `README.md` and `package.json` of that revision contain neither a licence
  statement nor a `license` field; the README describes the project itself as a
  non-original ("非原创") Vue 3 reimplementation.
- This project therefore attributes the adapted work under the MIT terms
  recorded for the adaptation, and reproduces them below. The text below is a
  **transcription**, not a copy of an upstream `LICENSE` file, because the
  verified revision has no such file.
- If the upstream author publishes a licence file or a different notice, that
  notice takes precedence and should replace this section.

### MIT License

Copyright (c) 2026 guohaolian

(The year is taken from the upstream commit date above; the copyright holder is
identified by the upstream GitHub account because the upstream repository states
no separate copyright line. This text is transcribed for attribution.)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
