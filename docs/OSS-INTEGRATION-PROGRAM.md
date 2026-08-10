# OSS-integrasjonsprogram — SundayEdit × Clypra m.fl.

> Vedtatt 2026-08-09. Eier styrer tempo: si «kjør etappe N». Fable dirigerer,
> Opus/Sonnet utfører. Hver etappe gates på full grønn suite (npm run check +
> build + Playwright + ekte-ffmpeg der relevant) før merge.
>
> **Status 2026-08-10: E1–E6 og E8 er LEVERT. E4b og E7 er bevisst ikke gjort
> (henholdsvis eierbeslutning og betinget).** Sluttrapporten med målinger,
> eierbeslutninger og rigg-sjekkliste er `docs/OSS-PROGRAM-REPORT.md`.

## Bakgrunn og kilder

Research 08-09 (2 agenter, dybde + bredde) over GitHub-økosystemet rundt
Tauri/React-videoredigering. Hovedkilde: **AIEraDev/Clypra** (MIT, ~3k★,
samme stack som oss — CapCut-klone). Kvalitetsbilde: AI-assistert kodebase
med gjentakende mønster «backend komplett, UI-wiring fraværende» (keyframes
ødelagte, drag omgår undo, fake cloud-eksport) — MEN med flere genuint
utmerkede, rene, testede moduler. Sekundærkilder: **bilibili/WebAV** (MIT,
WebCodecs-kompositor), **jassub** (WebGL-libass — **LGPL-2.1+/FTL, ikke MIT**, se E4), **@clypra-studio/
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

### E1 — Fundament: løft + NOTICES _(S/M · Opus+Sonnet)_ ✅ LEVERT 2026-08-10 (`c347a01`)

Løft fra Clypra (read-only-klone finnes i scratchpad; verifiser mot upstream
`AIEraDev/Clypra@main`): `PlaybackClock.ts` (AudioContext-klokke, generasjons-
vakt, stall-kompensasjon), decide/execute-formen fra `PreviewPlaybackScheduler`
(ren `reconcile() → MediaAction[]` — **våre toleranser: ≤1 frame, ikke deres
0,5–2 s**), `animation.ts` (Newton-Raphson cubic-bezier + interpolasjon),
`transform/calculator.ts` (gizmo-matte, 474 linjer, ren). Opprett
`THIRD-PARTY-NOTICES.md`. Porter/skriv enhetstester for alt. INGEN UI-endring.
**DoD:** alle løft har tester; full gate grønn; NOTICES komplett.

### E2 — Preview-soliditet _(M · Opus)_ ✅ LEVERT 2026-08-10 (`17f9c12`)

`PlaybackClock` erstatter rAF-akkumulatoren som tidslinje-klokke (behold
`playheadMs`-kontrakten utad). Media-element-pool for flerklipps-forhånds-
visning (kilde-keyet, survives splits) drevet av reconcile-executor.
Adaptiv kvalitetsstige (Idle/Playback/Interaction-trinn) erstatter statisk
480p-proxy-adferd; frame-skipping ved shuttle >1×. **DoD:** A/V-drift ≤1
frame i testene; mediaSync-suiten utvidet; Playwright grønn; ingen regresjon
i caption-editoren.

### E3 — Tidslinje-UX-pakka _(M · Sonnet, Opus på gap-ops)_ ✅ LEVERT 2026-08-10 (`17f9c12`, ADR-011/012)

Forankret zoom (punkt under playhead/cursor står fast). Drag-polish:
skjermpiksel-kantssoner (8 px uansett zoom) + innsettings-hysterese.
Gap-motor som **Rust-ops** (detect/insert/remove/pack m/ beskyttede gap —
imiter `gapEngine.ts`, implementer i `timeline_ops.rs`-mønsteret). Filmstripe
på klippbokser: utvid `extract_thumbnail` til stripe-tiles på **fast grid per
zoomnivå** (gjenbruk ved zoom — Clypras beste innsikt; anvend samme lekse på
waveform-cachen, som Clypra selv glemte). **DoD:** nye ops m/ tester; tiles
gjenbrukes på tvers av zoomsteg (test); full gate.

### E4 — Karaoke-captions 🏆 _(M · Opus — flaggskipet)_ ✅ E4a LEVERT 2026-08-10 (`4656139`) · ⛔ E4b IKKE GJORT (eierbeslutning)

> ⚠️ **Lisenskorreksjon 08-09 (viktig):** programmet oppga først jassub som
> MIT. Det er FEIL — npm-metadata sier `LGPL-2.1-or-later AND (FTL OR
GPL-2.0-or-later) AND MIT AND …` fordi pakken bunter libass + FreeType +
> fribidi. For et lukket kommersielt produkt betyr LGPL konkrete plikter
> (erstattbar/dynamisk lenket komponent, kildetilbud, attribusjon). Vi har
> allerede samme kategori via ffmpeg-sidecar, men å legge til en LGPL-
> avhengighet er en **eierbeslutning**, ikke en nattbeslutning.

**Derfor todelt E4:**

- **E4a (kjøres — null lisensrisiko):** `write_ass` får karaoke-tags
  (`\k`/`\kf`) generert fra word-timings vi allerede har, + valgfri
  confidence-farging. Preview-siden rendrer karaoke i VÅRT eget
  canvas-overlegg direkte fra de samme word-timingene (vi eier begge
  sider). Paritet sikres med felles ren timing-funksjon (én kilde til
  `word → (start,varighet,tilstand)`) + ekte-ffmpeg-test som brenner ASS
  og sammenligner mot forventet frame-tilstand. Stil-UI: karaoke av/på +
  highlight-stil per stilpreset.
- **E4b (FORBEREDES, ikke installert):** jassub som eksakt-libass-preview
  bak et flagg — dokumenter LGPL-pliktene og la eier bestemme. Vår
  ffmpeg-preview-proxy gir allerede ekte libass-fasit ved behov.

**DoD (E4a):** ASS-snapshot-tester; canvas↔ASS-paritet testet via den delte
timing-funksjonen; ekte-ffmpeg burn-in-test; HELE caption-suiten grønn
(flaggskip-gate); ytelse: karaoke-rendring belaster ikke playhead-loopen.

### E5 — GPU-kompositor-spike _(M · Opus — beslutningsetappe)_ ✅ LEVERT 2026-08-10 (`4656139`, ADR-010)

Prototyp bak capability-flagg: PixiJS-kompositor matet av skjult-video-pool
(Clypras beviste modell) vs WebAV/mediabunny-sti. Mål i ekte Tauri-WKWebView:
frame-troskap, seek-latens, minne, stabilitet. **Leveranse er en BESLUTNING**
(ADR-010: kompositorvalg + frame-kilde-strategi), ikke produksjonskode.
**DoD:** målbar sammenligning dokumentert; ADR-010 skrevet; eier godkjenner
retning før E6.

### E6 — Effektbibliotek _(L · Opus+Sonnet — krever E5-vedtak)_ ✅ LEVERT 2026-08-10 (ADR-013)

> **Avvik fra planen, med vilje:** `@clypra-studio/engine` ble **ikke**
> installert. 233 GPU-effekter er 229 måter å love noe eksporten ikke kan
> levere — og «det du eksporterer er det du så» er et produktløfte, ikke en
> detalj. Vi installerte `pixi.js@^8` (kompositoren fra ADR-010) og bygde
> **porten** i stedet for biblioteket: et kuratert register der hver effekt har
> en ffmpeg-ekvivalent. Se ADR-013. Katalogen kan revurderes når E7s hybride
> eksport gjør preview-only-effekter renderbare.

Levert:

- **UA-fiksen fra ADR-010** (forutsetningen): `src-tauri/tauri.macos.conf.json`
  setter `Version/17.0 Safari/605.1.15`-token — kun macOS, vakttestet i
  `tests/webview_user_agent.rs` (speiler Pixis `isSafari()`-regex + drift-vakt
  mot at plattform-config ERSTATTER `windows`-arrayet).
- **Pixi-kompositor bak kapabilitetsflagg, AV som standard**
  (`src/features/timeline/compositor/`): persistert brukervalg + WebGL2-probe
  med automatisk av-bryter. `pixi.js` lastes dynamisk (egen chunk), og med
  flagget av er preview-DOM-en bevist identisk med før E6.
- **Kuratert register** (brightness/contrast/saturation/grayscale) i både Rust
  og TS, koblet inn i `compose.rs`-kjeden (farge før geometri) og i
  ClipInspector (undoable via `store.run`, i18n ×7).
- **Paritet mot ekte ffmpeg**: `tests/effects_ffmpeg_parity.rs` rendrer hver
  effekt og MÅLER resultatet med `signalstats` (YAVG/SATAVG) — en filter som
  parser men ikke gjør noe består ikke.

**DoD:** kuratert subsett parity-testet mot ekte ffmpeg; ikke-kuraterte
effekter skjult; full gate.

### E7 — Hybrid eksport _(L · Opus)_ ⛔ UTSATT 2026-08-10 — BETINGET, ikke «gjenstår»

For GPU-effekter uten ffmpeg-ekvivalent: rendre KUN de berørte segmentene via
kompositor-readback til mellomfiler, som mates inn som inputs i eksisterende
`filter_complex`-graf (basen forblir ren ffmpeg). Fremdrift/avbryt bevart.
**DoD:** ekte-ffmpeg-tester for hybridgraf; determinisme-test (to kjøringer,
identisk ffprobe-metadata); eksportløfte-dokumentasjon oppdatert.

> **Hvorfor den ikke er kjørt:** E7 løser nøyaktig ett problem — effekter som
> ffmpeg ikke kan uttrykke. Det kuraterte registeret fra E6 (ADR-013) har per
> definisjon ingen slike: hver effekt er tatt inn NETTOPP fordi begge sider kan
> produsere den, og pariteten er målt mot ekte ffmpeg (`signalstats`), ikke mot
> en streng. E7 har altså ingen jobb i dagens produkt, og å bygge den nå ville
> vært å legge til en andre eksportsti uten en eneste bruker.
>
> **Utløseren som gjør den nødvendig:** første effekt eller overgang vi vil ha
> som ffmpeg ikke kan uttrykke (glød, partikler, blur-typer utenfor
> `gblur`/`unsharp`, ikke-`xfade`-overganger). Da — og først da — er E7 neste
> etappe. Til den dagen er «utvid katalogen» og «bygg E7» samme beslutning.

### E8 — Herding + drift _(M · flere Sonnet + Opus-verifikatorer)_ ✅ LEVERT 2026-08-10

Skjøtefeil-runde over de nye sømmene (klokke↔scheduler, ASS preview↔eksport,
kompositor↔ffmpeg-parity, tiles↔zoom), rigg-testliste oppdatert (SMOKE-TEST
E-rader), programrapport, memory. **DoD:** funn fikset m/ regresjonstester;
full gate; rapport.

Levert:

- **Kjørbar speilparitet** (`src-tauri/tests/mirror_fixture_parity.rs` +
  `src/lib/mirrorParity.test.ts` + generert fixture): Rust-siden kjøres over en
  adversariell tabell, inndata OG utdata fryses, og TS-speilene må reprodusere
  dem eksakt. Erstatter «hold disse i lockstep»-kommentarer og
  kildetekst-grepping med 199 kjørende tester over karaoke-stigen,
  tile-gridet og effekt-fragmentene. **Null drift funnet** — de tre mistenkte
  speilene er avkreftet OG pinnet.
- **3 ekte funn fikset**, hver med en test som feiler før fiksen og er
  mutasjonsverifisert: filmstripen dukket aldri opp uten en urelatert
  interaksjon (memo uten avhengighet til tile-cachen); en grov stedfortreder
  ble tegnet klemt inn i barnets rektangel og gjentatt per søsken; og
  `describeScene`s `unsupported` ble beregnet men aldri vist, så previewen
  tegnet beskjæring og lagstabler stille feil.
- **4 mistanker avkreftet** med begrunnelse (kvalitetsstige→eksport,
  klokke↔stride, reconcile↔elementtilstander, flagg-av-stien).

Detaljer, målinger og rigg-sjekkliste: **`docs/OSS-PROGRAM-REPORT.md`**.

## Utenfor programmet (notert, ikke besluttet)

- ebur128 (Rust loudness/normalisering) — kandidat til senere lyd-etappe.
- wavesurfer.js — vi har eget waveform-system; kun idé-kilde.
- whisperX forced-alignment — arkitektur-referanse for enda bedre word-timing.
- ez-ffmpeg (FFI) — WATCH; pre-1.0, ville erstattet sidecar-arkitekturen.
- Clypras native-eksport-«eligibility gate» — allerede dekket av vår
  simple-path; mønsteret gjenbrukes om gaten skal utvides.

## Kildekart (løft → fil)

| Vi bygger              | Fra                                                                      | Lisens      |
| ---------------------- | ------------------------------------------------------------------------ | ----------- |
| Tidslinje-klokke       | `Clypra src/core/playback/PlaybackClock.ts`                              | MIT         |
| Sync-policy-form       | `Clypra src/core/playback/PreviewPlaybackScheduler.ts` (kun formen)      | MIT         |
| Keyframe-matte         | `Clypra src/core/evaluation/animation.ts`                                | MIT         |
| Gizmo-matte            | `Clypra src/components/editor/transform/calculator.ts`                   | MIT         |
| Gap-semantikk          | `Clypra src/lib/timeline/gapEngine.ts` (imiteres i Rust)                 | idé         |
| Tile-grid-innsikt      | `Clypra` thumbnail-system (imiteres)                                     | idé         |
| Karaoke-rendring       | eget canvas-overlegg + `write_ass` (E4a) — INGEN ny avhengighet          | —           |
| ~~Karaoke via libass~~ | `ThaUnknown/jassub` — **LGPL-2.1+/FTL, IKKE MIT** (E4b = eierbeslutning) | LGPL m.fl.  |
| GPU-kompositor         | `pixi.js@^8` (npm-avhengighet, E6)                                       | MIT         |
| ~~Effektmotor~~        | `@clypra-studio/engine` — **avvist i E6**, kuratert register i stedet    | MIT         |
| WebCodecs-kilde        | `bilibili/WebAV` / `Vanilagy/mediabunny` (E5-spike)                      | MIT/MPL-2.0 |
