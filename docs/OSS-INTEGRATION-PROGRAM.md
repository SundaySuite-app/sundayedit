# OSS-integrasjonsprogram — SundayEdit × Clypra m.fl.

> Vedtatt 2026-08-09. Eier styrer tempo: si «kjør etappe N». Fable dirigerer,
> Opus/Sonnet utfører. Hver etappe gates på full grønn suite (npm run check +
> build + Playwright + ekte-ffmpeg der relevant) før merge.

## Bakgrunn og kilder

Research 08-09 (2 agenter, dybde + bredde) over GitHub-økosystemet rundt
Tauri/React-videoredigering. Hovedkilde: **AIEraDev/Clypra** (MIT, ~3k★,
samme stack som oss — CapCut-klone). Kvalitetsbilde: AI-assistert kodebase
med gjentakende mønster «backend komplett, UI-wiring fraværende» (keyframes
ødelagte, drag omgår undo, fake cloud-eksport) — MEN med flere genuint
utmerkede, rene, testede moduler. Sekundærkilder: **bilibili/WebAV** (MIT,
WebCodecs-kompositor), **jassub** (MIT, WebGL-libass), **@clypra-studio/
engine** (MIT på npm, 233 effekter/overganger, krever pixi.js@^8),
**mediabunny** (MPL-2.0). Full analyse i agent-rapportene (se git-historikk
for denne fila + memory).

**Sikkerhet:** `Elwininvading138/Clypra` og lignende lav-stjerne-kloner er
bekreftet malware-agn-mønster — kun `AIEraDev/*` og de navngitte repoene
over er godkjente kilder. Les alltid LICENSE-fila, ikke README (Twick-fella:
«Sustainable Use License» bak MIT-lignende markedsføring).

## Arkitekturvedtak (låst for programmet)

1. **ffmpeg `filter_complex` forblir eksport-SANNHETEN.** Rask, deterministisk,
   headless-testbar (18 ekte-ffmpeg-tester). Clypras modell (GPU-kompositor →
   readback → ffmpeg som ren encoder) adopteres IKKE for hele tidslinjen.
2. **GPU-preview utforskes nå** (eiervalg) — bak capability-sjekk, med den
   pragmatiske `<video>`-stien som fallback. Kompositorvalg (PixiJS vs WebAV)
   avgjøres empirisk i E5-spiken; effektbiblioteket (E6) forutsetter PixiJS,
   så PixiJS er favoritt med WebCodecs/mediabunny som frame-kilde-oppgradering.
3. **WYSIWYG-spenningen løses i lag:** E6 kuraterer effekter/overganger med
   ffmpeg-ekvivalent (parity-testet); E7 bygger hybrid eksport (GPU-rendrede
   effektsegmenter som mellomklipp inn i ffmpeg-grafen) for resten. Captions
   er allerede ekte WYSIWYG via ASS/libass begge veier (E4 forsterker dette).
4. **Lisens:** MIT-løft med opphavsnotis i fila + `THIRD-PARTY-NOTICES.md`
   (etablert SundaySync-praksis). Ingen kode fra no-license-repoer.
5. **Flaggskip-vern:** captions/confidence-pipelinen må aldri regrere —
   caption-relaterte etapper (E4) har egen regresjonsgate på hele
   caption-testsuiten.

## Etapper

### E1 — Fundament: løft + NOTICES _(S/M · Opus+Sonnet)_

Løft fra Clypra (read-only-klone finnes i scratchpad; verifiser mot upstream
`AIEraDev/Clypra@main`): `PlaybackClock.ts` (AudioContext-klokke, generasjons-
vakt, stall-kompensasjon), decide/execute-formen fra `PreviewPlaybackScheduler`
(ren `reconcile() → MediaAction[]` — **våre toleranser: ≤1 frame, ikke deres
0,5–2 s**), `animation.ts` (Newton-Raphson cubic-bezier + interpolasjon),
`transform/calculator.ts` (gizmo-matte, 474 linjer, ren). Opprett
`THIRD-PARTY-NOTICES.md`. Porter/skriv enhetstester for alt. INGEN UI-endring.
**DoD:** alle løft har tester; full gate grønn; NOTICES komplett.

### E2 — Preview-soliditet _(M · Opus)_

`PlaybackClock` erstatter rAF-akkumulatoren som tidslinje-klokke (behold
`playheadMs`-kontrakten utad). Media-element-pool for flerklipps-forhånds-
visning (kilde-keyet, survives splits) drevet av reconcile-executor.
Adaptiv kvalitetsstige (Idle/Playback/Interaction-trinn) erstatter statisk
480p-proxy-adferd; frame-skipping ved shuttle >1×. **DoD:** A/V-drift ≤1
frame i testene; mediaSync-suiten utvidet; Playwright grønn; ingen regresjon
i caption-editoren.

### E3 — Tidslinje-UX-pakka _(M · Sonnet, Opus på gap-ops)_

Forankret zoom (punkt under playhead/cursor står fast). Drag-polish:
skjermpiksel-kantssoner (8 px uansett zoom) + innsettings-hysterese.
Gap-motor som **Rust-ops** (detect/insert/remove/pack m/ beskyttede gap —
imiter `gapEngine.ts`, implementer i `timeline_ops.rs`-mønsteret). Filmstripe
på klippbokser: utvid `extract_thumbnail` til stripe-tiles på **fast grid per
zoomnivå** (gjenbruk ved zoom — Clypras beste innsikt; anvend samme lekse på
waveform-cachen, som Clypra selv glemte). **DoD:** nye ops m/ tester; tiles
gjenbrukes på tvers av zoomsteg (test); full gate.

### E4 — Karaoke-captions 🏆 _(M · Opus — flaggskipet)_

`write_ass` utvides med karaoke-tags (`\k`/`\kf`) fra word-timings vi allerede
har + valgfri confidence-farging. **jassub** (MIT WebGL-libass) rendrer samme
ASS i preview → ekte WYSIWYG (libass er motoren begge veier). Stil-UI:
karaoke av/på + highlight-stil per stilpreset. **DoD:** ASS-snapshot-tester;
jassub-preview mot eksportert burn-in visuelt verifisert (ekte-ffmpeg-test +
screenshot-sammenligning); HELE caption-suiten grønn (flaggskip-gate);
ytelse: jassub belaster ikke playhead-loopen (>30 fps preview).

### E5 — GPU-kompositor-spike _(M · Opus — beslutningsetappe)_

Prototyp bak capability-flagg: PixiJS-kompositor matet av skjult-video-pool
(Clypras beviste modell) vs WebAV/mediabunny-sti. Mål i ekte Tauri-WKWebView:
frame-troskap, seek-latens, minne, stabilitet. **Leveranse er en BESLUTNING**
(ADR-010: kompositorvalg + frame-kilde-strategi), ikke produksjonskode.
**DoD:** målbar sammenligning dokumentert; ADR-010 skrevet; eier godkjenner
retning før E6.

### E6 — Effektbibliotek _(L · Opus+Sonnet — krever E5-vedtak)_

`npm install @clypra-studio/engine` (+shaders/types; verifiser at registry er
selvforsynt offline — deler av Clypra-APPENS katalog hentes fra privat API,
men engine-tarballen bærer de 233 id-ene). Monter på valgt kompositor.
**Kuratér startsubsett** med ffmpeg-ekvivalent (xfade-navn, eq/hue/curves
osv.) + parity-tester preview↔eksport per effekt. Effektpanel i
ClipInspector (undoable via store). **DoD:** kuratert subsett parity-testet
mot ekte ffmpeg; ikke-kuraterte effekter skjult; full gate.

### E7 — Hybrid eksport _(L · Opus)_

For GPU-effekter uten ffmpeg-ekvivalent: rendre KUN de berørte segmentene via
kompositor-readback til mellomfiler, som mates inn som inputs i eksisterende
`filter_complex`-graf (basen forblir ren ffmpeg). Fremdrift/avbryt bevart.
**DoD:** ekte-ffmpeg-tester for hybridgraf; determinisme-test (to kjøringer,
identisk ffprobe-metadata); eksportløfte-dokumentasjon oppdatert.

### E8 — Herding + drift _(M · flere Sonnet + Opus-verifikatorer)_

Skjøtefeil-runde over de nye sømmene (klokke↔scheduler, ASS preview↔eksport,
kompositor↔ffmpeg-parity, tiles↔zoom), rigg-testliste oppdatert (SMOKE-TEST
E-rader), programrapport, memory. **DoD:** funn fikset m/ regresjonstester;
full gate; rapport.

## Utenfor programmet (notert, ikke besluttet)

- ebur128 (Rust loudness/normalisering) — kandidat til senere lyd-etappe.
- wavesurfer.js — vi har eget waveform-system; kun idé-kilde.
- whisperX forced-alignment — arkitektur-referanse for enda bedre word-timing.
- ez-ffmpeg (FFI) — WATCH; pre-1.0, ville erstattet sidecar-arkitekturen.
- Clypras native-eksport-«eligibility gate» — allerede dekket av vår
  simple-path; mønsteret gjenbrukes om gaten skal utvides.

## Kildekart (løft → fil)

| Vi bygger         | Fra                                                                 | Lisens      |
| ----------------- | ------------------------------------------------------------------- | ----------- |
| Tidslinje-klokke  | `Clypra src/core/playback/PlaybackClock.ts`                         | MIT         |
| Sync-policy-form  | `Clypra src/core/playback/PreviewPlaybackScheduler.ts` (kun formen) | MIT         |
| Keyframe-matte    | `Clypra src/core/evaluation/animation.ts`                           | MIT         |
| Gizmo-matte       | `Clypra src/components/editor/transform/calculator.ts`              | MIT         |
| Gap-semantikk     | `Clypra src/lib/timeline/gapEngine.ts` (imiteres i Rust)            | idé         |
| Tile-grid-innsikt | `Clypra` thumbnail-system (imiteres)                                | idé         |
| Karaoke-rendring  | `ThaUnknown/jassub` (npm-avhengighet)                               | MIT         |
| Effektmotor       | `@clypra-studio/engine` (npm-avhengighet, E6)                       | MIT         |
| WebCodecs-kilde   | `bilibili/WebAV` / `Vanilagy/mediabunny` (E5-spike)                 | MIT/MPL-2.0 |
