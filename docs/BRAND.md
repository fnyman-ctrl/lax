# Brand

Lax sees everything on your screen. That can feel unsettling, so the brand's job
is to make Lax feel like a calm, friendly companion, never like a camera.

## The idea

Salmon (*lax* in Swedish) travel out to sea and, years later, find their way back
to the exact river they were born in. Lax does the same with what you do on your
computer: it remembers where you've been, and it stays home.

## The mascot: a silver salmon

![The Lax app icon](../src-tauri/icons/128x128@2x.png)

A salmon as it looks at sea, on its way home:

- **Silver-blue, never orange.** Blue-grey back, silver sides, white belly. Salmon
  pink only in the cheek and the thin stripe along the side.
- **Streamlined,** with a forked tail and the small adipose fin on the back that only
  salmon and their relatives have.
- **Swims diagonally upwards,** against the current. Only lies flat when it sleeps.
- **A friendly cartoon eye** with a white and a highlight. Never a staring or watchful
  eye, and it never looks straight at the viewer.
- **Soft, round shapes only.** No teeth, no sharp fins.
- **Moods:** awake (capturing), asleep with "zzz" (paused), waving (onboarding).

Lax is an alternative to apps like Goldfish, so it must never be mistaken for one:
no round orange fish.

## Colours

| Name | Hex | Use |
|---|---|---|
| Djupvatten (deep water) | `#0F2A33` | Text, outlines, dark backgrounds |
| Strömfåra (current) | `#2E6F73` | Primary buttons, links, accents |
| Ryggblå (back blue) | `#4F7F93` | The salmon's back and fins |
| Silversida (silver side) | `#C7D6DC` | The salmon's sides, borders |
| Älvsten (river stone) | `#EEF2F1` | Light backgrounds |
| Laxrosa (salmon pink) | `#E07A84` | Sparingly: the cheek, small highlights |

Text and backgrounds must meet WCAG contrast (4.5:1 for body text).

## Type

- **Bricolage Grotesque** for headings
- **Atkinson Hyperlegible** for body text, designed to be readable with low vision
- **JetBrains Mono** for data and code

All three use the SIL Open Font License. They must be bundled with the app, never
loaded from Google Fonts at runtime (GUARDRAILS §7: no network requests).

## Voice

Calm, concrete and honest. Say exactly what happens ("Paused until 14:15",
"Everything stays on this Mac"). No hype ("your AI-powered memory experience!") and
no reassurance without substance ("don't worry about privacy"). Only promise what
the code keeps, and point to [CLAIMS.md](CLAIMS.md).

## Icons

Every icon is drawn from an SVG source in the repository:

| Source | Becomes |
|---|---|
| `src-tauri/icons/app-icon.svg` | The app icon in every size (`icon.icns`, `icon.ico`, PNGs) |
| `src-tauri/icons/tray/active.svg` | The menu bar icon while capturing: the salmon silhouette |
| `src-tauri/icons/tray/paused.svg` | The menu bar icon while paused: a faded salmon with pause bars |

After changing a source, run `npm run icons` and commit the result. The menu bar
icons are macOS template images: black on transparent, recoloured by macOS.
