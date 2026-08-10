# OSS-integrasjonsprogram — sluttrapport E1–E6 (+ E8-herding)

**Skrevet 2026-08-10.** Programmet er `docs/OSS-INTEGRATION-PROGRAM.md`;
beslutningene er ADR-010 til ADR-013 i `docs/DECISIONS.md`. Denne rapporten er
statusdokumentet: hva som ble levert, hva målingene faktisk viser, hva vi med
vilje IKKE gjorde, hva eier må bestemme, og hva som må testes på rigg.

Rapporten er skrevet på norsk fordi eier leser den; koden, kommentarene og
ADR-ene er på engelsk som ellers i repoet.

---

## 1. Kortversjon

| Etappe                      | Status                         | Kjerneleveranse                                                                          |
| --------------------------- | ------------------------------ | ---------------------------------------------------------------------------------------- |
| **E1** Fundament + NOTICES  | ✅ 2026-08-10 (`c347a01`)      | 4 rene moduler løftet fra Clypra (MIT) + `THIRD-PARTY-NOTICES.md`. Null UI-endring.      |
| **E2** Preview-soliditet    | ✅ 2026-08-10 (`17f9c12`)      | Lydklokke som tidslinje-klokke, reconcile-policy i preview, adaptiv kvalitetsstige.      |
| **E3** Tidslinje-UX         | ✅ 2026-08-10 (`17f9c12`)      | Forankret zoom, drag-polish, gap-ops i Rust, filmstripe på fast tile-grid (ADR-011/012). |
| **E4a** Karaoke-captions 🏆 | ✅ 2026-08-10 (`4656139`)      | `\k`/`\kf` i ASS + canvas/DOM-overlegg, begge fra ÉN delt timing-kilde.                  |
| **E4b** jassub (libass)     | ⛔ Ikke gjort — eierbeslutning | LGPL-plikter, se §4.1.                                                                   |
| **E5** Kompositor-spike     | ✅ 2026-08-10 (`4656139`)      | ADR-010: PixiJS 8 valgt, WebCodecs utsatt. Målt i ekte WKWebView.                        |
| **E6** Effektbibliotek      | ✅ 2026-08-10 (denne grenen)   | UA-fiksen, Pixi-kompositor bak flagg (AV som standard), kuratert effektregister.         |
| **E7** Hybrid eksport       | ⛔ Utsatt — betinget           | Trengs ikke for det kuraterte subsettet. Se §4.2.                                        |
| **E8** Herding              | ✅ 2026-08-10 (denne grenen)   | Skjøtefeil-runde: 3 funn fikset, 4 avkreftet, ny kjørbar speilparitet-test.              |

Full grønn port ved skriving: **vitest 916**, **cargo 740 + 52 integrasjon**,
**Playwright 58**, `npm run build`, clippy, eslint, tsc.

---

## 2. Hva E1–E6 leverte

### E1 — fundamentet (ingen synlig endring)

Fire moduler løftet fra `AIEraDev/Clypra` (MIT, commit `2e85676f`), hver med
opphavsnotis i fila og full tekst i `THIRD-PARTY-NOTICES.md`:

- `playbackClock.ts` — AudioContext-klokke med generasjonsvakt og
  stall-kompensasjon. **Sju bevisste avvik fra kilden** er dokumentert i
  fil-toppen; de viktigste: millisekunder overalt (Clypra bruker sekunder),
  fryser aldri på en suspendert AudioContext (Clypra returnerer stale tid),
  fortegnsatt rate for J/K/L-shuttle, og ingen modul-singleton.
- `mediaReconcile.ts` — decide/execute-FORMEN er løftet, tallene er våre:
  Clypra tolererer 0,5–2 s A/V-drift, vi budsjetterer **én frame** (33,3 ms
  ved 30 fps) under avspilling og **en halv frame** parkert. Clypras
  tidsbaserte seek-rate-limit (som låser drift ute i opptil 1,5 s) er byttet
  mot en tilstandsbasert vakt: så lenge elementet melder `seeking` sender vi
  ingen ny seek.
- `animation.ts` — Newton-Raphson cubic-bezier + interpolasjon.
- `gizmoCalculator.ts` — transform-håndtaksmatte (474 linjer, ren).

### E2 — preview-soliditet

`PlaybackClock` er nå tidslinjens klokke i `Timeline.tsx`; rAF bestemmer bare
NÅR vi ser på klokka. `MediaPlayer` snapshotter sitt `<video>`, kaller
`reconcileMedia` og utfører aksjonene — policyen er ren og testes headless.
`previewQuality.ts` innførte en eksplisitt kvalitetsstige (idle 100 % /
playback 50 % / interaction 25 % / export 100 %) og `renderStride`, som holder
arbeidet per veggklokkesekund flatt under shuttle.

**Eksportstien er urørt.** Stigen når kun `renderPreviewProxy`; sluttleveransen
går via `compose.render` + `defaultComposeSettings(project)` fra det uendrede
prosjektet. Dette er ettergått i kode og dekket av test — se §3.5.

### E3 — tidslinje-UX

Gap-motoren (`detect_gaps` / `insert_gap_with_ripple` /
`remove_gap_with_ripple` / `pack_track`) ligger i Rust. **ADR-011:** beskyttede
gap er DERIVERT, ikke lagret — markøren er `TimelineItem.locked`, så ingen
skjemaendring og ingenting å søppelsamle.

**ADR-012:** filmstripe-tiles (og senere waveform) adresseres på et absolutt
grid per zoomnivå — tier `t` har spenn `64 000 >> t` ms, tile `i` dekker
`[i·spenn, (i+1)·spenn)` forankret i tidslinje-null. 64 s er valgt fordi hver
tier da halveres EKSAKT, som er det som gjør at tiles nøstes: tile `i` på tier
`t` er nøyaktig tiles `2i` og `2i+1` på tier `t+1`. Konsekvens: panorering
gjenbruker tiles, og zooming kan vise en grovere forelder mens barnet rendres.

### E4a — karaoke 🏆 (flaggskipet)

`src-tauri/src/services/karaoke.rs` er **sannhetskilden** for per-ord-timing.
Både `write_ass` (libass-innbrenning) og DOM-overlegget i preview leser derfra.

Grunnen til at det MÅ være én kilde: ASS `\k`-varigheter er **kumulative** —
libass summerer dem fra Dialogue `Start`. Ett centisekund avrundingsfeil på ord
3 forskyver hvert eneste ord etter det. Derfor beregnes varighetene fra en
kumulativ centisekund-stige forankret i captionens egne ASS-tidsstempler, aldri
ved å runde hvert ords eget spenn. Invarianten `sum(duration_cs) == Dialogue-
spennet i cs` holder eksakt på hele testtabellen, inkludert inverterte
captions, negative starttider, ordspenn med lengde null og ord utenfor
captionens grenser.

Karaoke er **AV som standard**, og med det av er ASS-utdata byte-identisk med
før E4a (pinnet av `ass_output_is_byte_identical_when_karaoke_off`).

### E5 — kompositor-beslutningen (ADR-010)

Målt direkte i **macOS WKWebView** — motoren Tauri faktisk bruker — med en
Swift-harness (`wkrun.swift`), ikke i Chromium. Scene: to 1080p30 H.264-lag,
øvre lag skalert 0,55 / rotert 8° / alpha 0,85, inn i 1920×1080 ved 30 fps.

| WKWebView, 2×1080p30 + transform | C: `<video>`+canvas2d | A: PixiJS 8 **som levert** | A′: PixiJS 8 **+ UA-fiks** | B: WebAV `MP4Clip` |
| -------------------------------- | --------------------- | -------------------------- | -------------------------- | ------------------ |
| oppstart → første frame          | 53 ms                 | 481 ms                     | 139 ms                     | 79 ms              |
| vedvarende fps (mål 30)          | 30,0 / 0 sene         | **20,2 / 43 sene**         | 30,0 / 0 sene              | 30,0 / 0 sene      |
| komposittid snitt / p95          | 0,5 / 1 ms            | **24,3 / 25 ms**           | 0,4 / 1 ms                 | 0,1 / 1 ms         |
| tilfeldig seek snitt / p95       | 12,2 / 22 ms          | 63 / 73 ms                 | 15,1 / 27 ms               | **65,1 / 125 ms**  |
| ±1 frame-steg, snitt             | 2,2 ms                | 52,8 ms                    | 3,4 ms                     | ~0 ms              |
| topp WebContent RSS              | 37 MB                 | 354 MB                     | 61 MB                      | 93 MB (10 s klipp) |
| bunt-kostnad (min+gzip)          | 0                     | 157,2 kB                   | 157,2 kB                   | 45,7 kB            |

**Funnet som avgjorde det — 42×-klippen.** Pixi sender
`forceAllocation = isSafari()` til sin video-opplaster, og `isSafari()` er et
userAgent-regex. Tauris WKWebView rapporterer
`Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)`
— **uten `Safari`-token** — fordi wry bare kaller `setCustomUserAgent` når
`user_agent` er konfigurert. Pixis WebKit-workaround fyrte derfor aldri:

| WKWebView, per frame, 2 lag           | Chromium (headless, SwiftShader) |
| ------------------------------------- | -------------------------------- |
| `texImage2D(video)` — **0,69 ms**     | 10,02 ms                         |
| `texSubImage2D(video)` — **28,92 ms** | 10,16 ms                         |

**42× i WebKit. Null forskjell i Chromium** — som er nøyaktig grunnen til at en
Chromium-basert benchmark ville bommet fullstendig på dette. Å pinne
`forceAllocation = true` tok scenen fra 25,64 ms til 0,59 ms per frame.

To målinger til fjernet det vanlige argumentet for WebCodecs:

- **Frame-kilden er ikke flaskehalsen.** Opplasting fra `<video>` kostet
  1,82 ms/frame; fra en WebCodecs `VideoFrame` 1,86 ms/frame. WebCodecs kjøper
  frame-eksakthet, ikke gjennomstrømning.
- **WebAVs dekodede frame-cache skalerer ikke gratis.** Gjennom et 60 s
  1080p-klipp: `MP4Clip` toppet på 235 MB RSS mot `<video>`-elementets 48 MB —
  per klipp, på en fler-spors tidslinje.

Spiken la ikke inn én eneste avhengighet: `package.json` og
`package-lock.json` var byte-identiske etterpå.

### E6 — porten, ikke biblioteket (ADR-013)

Fire leveranser:

1. **UA-fiksen** (forutsetningen fra ADR-010): `src-tauri/tauri.macos.conf.json`
   setter en UA som beholder WKWebViews ekte form og TILFØYER
   `Version/17.0 Safari/605.1.15 SundayEdit`. Kun macOS — Windows/WebView2 har
   sin egen Chromium-UA, der målingen viste null forskjell. Vaktet av
   `src-tauri/tests/webview_user_agent.rs`, som reimplementerer Pixis
   `isSafari()`-regex, sjekker at basiskonfigen IKKE setter noen UA, og at
   macOS-vindusdefinisjonen ikke har drevet fra basis (plattform-config
   ERSTATTER arrays — en ny egenskap i basisfila ville ellers forsvinne på macOS).
   Risikoen er navngitt presist i ADR-010: webviewet gjør **ingen eksterne
   nettkall** — alt HTTP går via Rust `reqwest` med sin egen UA.
2. **Pixi-kompositor bak kapabilitetsflagg, AV som standard.** To porter:
   et persistert brukervalg (`localStorage`) OG en WebGL2-probe med
   `failIfMajorPerformanceCaveat`. De lagres hver for seg med vilje — en
   automatisk fallback må ikke skrive om brukerens innstilling, ellers ser
   bryteren ut til å slå seg av selv og brukeren får aldri vite hvorfor.
   `pixi.js` lastes via `React.lazy` + dynamisk import; **verifisert i bygget**:
   entry-chunken (`index-*.js`, 662 kB) inneholder null `pixi`-referanser,
   renderer-chunken (856 kB) hentes bare når flagget er på.
3. **Kuratert register** — `brightness` / `contrast` / `saturation` /
   `grayscale`, definert både i Rust (`services/effects.rs`) og TS
   (`effects/registry.ts`), koblet inn i `compose.rs` sin item-kjede med
   **farge før geometri**, og i ClipInspector (angrbart via `store.run`, i18n ×7).
4. **Paritet mot ekte ffmpeg** — `tests/effects_ffmpeg_parity.rs` rendrer hver
   effekt og MÅLER resultatet med `signalstats` (YAVG/SATAVG). En filter som
   parser men ikke gjør noe består ikke.

---

## 3. E8 — skjøtefeil-runden

Metoden er den fra `reference-seam-bugs`: to lag som hver for seg er korrekte
og uenige i skjøten, begge med grønne tester. Hvert funn under er verifisert
ved å skrive en test som FEILER før fiksen og består etter, og hver fiks er
mutasjonstestet (endre koden tilbake → testen faller).

### 3.1 Nytt verktøy: kjørbar speilparitet

Tre skjøter har to implementasjoner av samme aritmetikk med vilje — én i Rust
(fordi eksporten bor der) og én i TS (fordi previewen bor der):

| skjøt     | Rust (sannhet)      | TypeScript (speil)                          |
| --------- | ------------------- | ------------------------------------------- |
| karaoke   | `services::karaoke` | `src/features/timeline/karaoke.ts`          |
| tile-grid | `services::tiles`   | `src/features/timeline/filmstrip.ts`        |
| effekter  | `services::effects` | `src/features/timeline/effects/registry.ts` |

Vaktene som fantes var enten **enhetstester av én side** eller
**kildetekst-assertions om den andre** (`effects_registry_parity.rs` grepper
etter regel-strenger i registry.ts). Begge lar hullet stå åpent: to halvdeler
som består hver sin tabell og er uenige om et inndata ingen av tabellene
tilfeldigvis inneholder. Kommentaren «hold disse i lockstep» er ikke en test.

Nytt: `src-tauri/tests/mirror_fixture_parity.rs` kjører RUST-siden over en
bevisst ekkel tabell (adversarielle tilfeller + fast-seedet sveip) og fryser
inndata OG utdata i `src/lib/__fixtures__/mirror-parity.json` (64
karaoke-tilfeller med 1 060 tilstands-samplinger, 138 tile-tilfeller, 91
effekt-tilfeller). `src/lib/mirrorParity.test.ts` spiller de identiske
inndataene gjennom TS-speilene og krever identiske svar — **199 kjørende tester**.

Begge retninger dekkes uansett hvilken suite som kjører først:

- Rust-adferd endres → committet fixture stemmer ikke → cargo-testen feiler og
  ber deg regenerere (`UPDATE_MIRROR_FIXTURE=1`).
- TS driver fra den frosne sannheten → vitest feiler.

Mutasjonstestet: `Math.floor`→`Math.round` i `toCs`, `end-1`→`end` i
`tilesForRange`, og `-0`-normaliseringen i tallformateringen ga til sammen 37
feilende tester (35 karaoke-tilfeller, tile-dekningen, effekt-fragmentene).

**Resultat: null drift funnet.** De tre speilene er verifisert like på hele
tabellen — inkludert invertert caption, negativ starttid, ord med
null-lengde-spenn, ord utenfor captionen, fencepost på tile-grensene, tier over
maks, og verdier som stresser tallformateringen. Mistanken om
karaoke-stige-uenighet og tile-grid-uenighet er dermed **avkreftet, og nå
pinnet**.

### 3.2 Funn 1 (ekte, fikset) — filmstripen dukket aldri opp

`useFilmstripTiles` memoiserte malelista på `[media, wanted, pxPerMs, item]`.
Tile-cachen lever UTENFOR React, så ingen av disse endrer seg når en tile blir
ferdig; `forceRender()` re-rendret, men memoen returnerte det tomme svaret fra
før lastingen. Strimmelen forble blank til noe urelatert (scroll, zoom, en
redigering) tilfeldigvis invaliderte memoen.

Den eksisterende testen bestod **ved uhell**: den sendte `clip()` — et ferskt
objektliteral — inn i render-callbacken, så `item`-referansen var ny hver
render. Ekte `ClipBox` sender item fra prosjekt-storen, som beholder identitet.

Bevist direkte: samme scenario med stabil `item` ga 0 tiles, med ferskt `item`
2 tiles.

**Fiks:** en `cacheEpoch`-teller som avanserer når en tile settler, med i
avhengighetslista. Ny test `paints tiles that arrive while nothing else
changes` holder `item` stabil.

### 3.3 Funn 2 (ekte, fikset) — grov stedfortreder tegnet på feil sted

`selectDisplayTiles` løser en tile som fortsatt laster til nærmeste ferdige
grovere FORFEDER (nøstings-egenskapen fra ADR-012). Men kalleren malte
forfederens JPEG i BARNETS rektangel. En forfeder dekker 2ⁿ× barnets
kildeområde, så hele det grove båndet ble klemt inn i en fjerdedel av bredden:
rammene som vises hører ikke hjemme der. Verre — hvert søskenbarn valgte samme
forfeder, så det samme klemte bildet ble malt fire ganger oppå seg selv, og
opasitetene la seg i lag til et lyst bånd.

**Fiks:** en stedfortreder males på SITT eget rektangel (klippboksens
`overflow-hidden` beskjærer det som stikker utenfor), og males **én gang** per
distinkt forfeder. Returtypen fikk `tier`/`index` så kalleren kan uttrykke
dette. Ny test asserterer at fem ønskede tier-2-tiles gir nøyaktig to
tier-0-stedfortredere, hver 1280 px bred og kant i kant.

### 3.4 Funn 3 (ekte, fikset) — previewen løy om det den ikke kan tegne

`describeScene` har fra dag én beregnet `unsupported` (`crop`,
`stacked-layers`), og ADR-013 sier at det «rapporteres … i stedet for å avvike
stille». Feltet ble beregnet, unit-testet — og **rendret ingen steder**. Med
flagget på tegnet en beskåret klipp seg ubeskåret og en stabel som bare sitt
øverste lag, og brukeren fant det ut ved eksport. På et produkt som selger
«det du eksporterer er det du så» er det den ene feilen man ikke har råd til.

**Fiks:** `approximationNotice(unsupported, t)` — ren, unit-testet — bygger
setningen, og kompositoren plasserer et lite merke. Tre i18n-nøkler ×7 språk.
rAF-løkka skriver til React-state bare når settet endrer seg, så en rolig frame
koster én strengsammenlikning. Canvas-en fikk sin egen container så et
React-styrt søsken aldri kan forstyrre en håndkoblet node.

### 3.5 Avkreftet (undersøkt, ingen feil)

- **`previewQuality` lekker inn i eksporten.** Nei. `quality.scalePct` når
  kun `renderPreviewProxy` (Timeline.tsx:997). Sluttleveransen går gjennom
  `ipc.compose.render(project, out, defaultComposeSettings(project))` fra det
  uendrede prosjektet, og `build_filter_complex` leser ingenting fra stigen.
  Dekket av `composeEngine.test.ts` («hands the project through UNMODIFIED»).
- **Klokke ↔ transport ↔ stride.** Gjennomgått. Frame-hopp-telleren nullstilles
  ved hver rate-endring OG eksplisitt i `seekTo`, så en seek midt i shuttle
  aldri blir usynlig i opptil ett stride. Klokkas egen `notify()` ved
  områdeslutt endrer raten til 0, som nullstiller telleren — sluttframen lander
  alltid. Ingen feil funnet.
- **`mediaReconcile` ↔ elementtilstander.** Gjennomgått mot utføreren i
  MediaPlayer. Den inaktive grenen får bevisst verken `speed` eller
  `hasBeenSeeked` (policyen bruker dem ikke der); `transportRate` settes til 0
  når elementet er forbi sitt eget domene, slik at policyen pinner framen
  istedenfor å rulle. Ingen feil funnet.
- **Flagg-av gjengir dagens adferd.** `MediaPlayer.compositor.test.tsx`
  asserterer den pre-E6-markupen bokstavelig (inkludert at elementet ikke får
  noe `style`-attributt i det hele tatt), at ingen kompositor monteres, og at
  den eksakte pre-E6-scenen kommer tilbake når flagget slås av igjen.
- **Avrunding av `x`/`y` i `scene.ts` vs `compose.rs`.** Rust bruker `f32::round`
  (halve bort fra null), JS `Math.round` (halve mot +∞). De er uenige kun når
  `bredde × transform.x` er nøyaktig `−0,5` — og previewen er uansett
  dokumentert som en tilnærming. Bevisst ikke endret: risiko uten gevinst.
- **Mellomrom mellom karaoke-ord.** I ASS hører separator-mellomrommet til
  FORRIGE `\k`-kjøring (slik at det ikke lyser opp sammen med ordet etter); i
  DOM-overlegget er det et ufarget tekstnode-mellomrom. Ingen synlig forskjell
  for noen stil vi støtter. Notert, ikke endret.

---

## 4. Hva som med vilje IKKE er gjort

### 4.1 E4b — jassub (libass i preview): **eierbeslutning**

Programmet oppga først jassub som MIT. Det er **feil**. npm-metadataene sier
`LGPL-2.1-or-later AND (FTL OR GPL-2.0-or-later) AND MIT AND …`, fordi pakken
bunter libass + FreeType + fribidi.

For et lukket kommersielt produkt betyr LGPL konkrete plikter: komponenten må
være erstattbar (dynamisk lenket eller utskiftbar av brukeren), kildetilbud må
gis, og attribusjon må følge med. Vi har allerede samme kategori via
ffmpeg-sidecaren — men **å legge til én til er en eierbeslutning, ikke en
nattbeslutning**. Derfor er den ikke tatt.

Praktisk konsekvens av å la den ligge: preview-karaoken er vår egen
DOM-gjengivelse (tro, men ikke pikselidentisk med libass). Trenger man
libass-fasit, finnes den allerede — preview-proxyen kjører ekte ffmpeg med
ekte libass.

### 4.2 E7 — hybrid eksport: **utsatt, betinget**

E7 skulle rendre GPU-effekter uten ffmpeg-ekvivalent som mellomfiler inn i
`filter_complex`-grafen. Det er **bare nødvendig for effekter som ikke har en
ffmpeg-ekvivalent** — og det kuraterte subsettet har det per definisjon: hver
eneste effekt i registeret er valgt fordi begge sider kan produsere den, og
pariteten er målt mot ekte ffmpeg. E7 har altså ingen jobb å gjøre i dag.

E7 blir aktuell i det øyeblikket katalogen skal utvides med noe ffmpeg ikke kan
uttrykke (blur-typer, glød, partikler, komplekse overganger). Det er derfor den
står som **betinget**, ikke som «gjenstår».

### 4.3 `@clypra-studio/engine`: **ikke installert**

233 GPU-effekter er 229 måter å love noe eksporten ikke kan levere. Vi
installerte `pixi.js@^8` (kompositoren ADR-010 valgte) og bygde **porten** i
stedet for biblioteket. Se ADR-013 for hele resonnementet.

### 4.4 Ikke målt (må ikke refereres som om det var det)

Fra ADR-010, uendret og fortsatt sant:

- **Frame-troskap.** Latens og gjennomstrømning er målt; om `<video>`-elementet
  lander på NØYAKTIG den framen tidslinja ba om er ikke målt. Det er det ekte
  argumentet for WebCodecs, og det står fortsatt åpent.
- **Den bygde app-en.** Alle WKWebView-tall kommer fra `wkrun.swift` over
  `http://127.0.0.1`, ikke fra SundayEdit.app over `tauri://`. **UA-en er lest
  fra wry 0.55.1 + vår config, ikke observert i den kjørende appen** — rigg-rad
  E6a er det eneste beviset.
- **Windows/Linux.** WebView2 og WebKitGTK er ikke probet i det hele tatt.
  Kapabilitetsporten bør ikke åpnes der før de er målt.
- **HDR, >2 lag, 4K.** Alt er 1080p SDR med to lag.
- **Chromium som ytelsesreferanse.** Playwright-kjøringen var headless med
  programvare-GL; absoluttallene betyr ingenting.

---

## 5. Nye eierbeslutninger

| #     | Beslutning                               | Anbefaling                                                                                                                                                                                                                                                                                                                                                                      |
| ----- | ---------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **1** | **Beholde UA-endringen?**                | **Ja.** Den er forutsetningen for hele E6 (42×). Formen bevarer WKWebViews ekte identitet og TILFØYER bare et `Safari`-token. Risikoen er navngitt: webviewet gjør ingen eksterne nettkall. Rev hvis vi noen gang laster tredjeparts webinnhold inn i webviewet.                                                                                                                |
| **2** | **GPU-flaggets standard — av eller på?** | **Behold AV.** ADR-010 sier det rett ut: preview-YTELSE alene rettferdiggjør ikke en GPU-kompositor (kontroll C er raskere og bruker 37 MB). Det kompositoren kjøper er at transform og effekter endelig VISES live. Slå den på som standard først når rigg-radene E6a–E6g er grønne på ekte maskinvare, og helst etter at frame-troskap er målt.                               |
| **3** | **Flere effekter i registeret?**         | **Ikke nå.** Regelen som holder løftet er «hver effekt må ha en ffmpeg-ekvivalent som er paritetstestet mot ekte ffmpeg». Naturlige neste kandidater innenfor den regelen: `gamma` (`eq=gamma=`), `hue` (`hue=h=`), `sharpen` (`unsharp=`), `blur` (`gblur=`) og `vignette`. Hver koster ~40 linjer × 2 sider + en signalstats-måling. Alt utenfor den regelen krever E7 først. |
| **4** | **E4b / jassub — LGPL?**                 | **Trenger ikke tas nå.** Preview-proxyen gir allerede ekte libass-fasit ved behov. Ta beslutningen den dagen pikselidentisk live-karaoke faktisk etterspørres.                                                                                                                                                                                                                  |
| **5** | **Åpne kapabilitetsporten på Windows?**  | **Nei, ikke før WebView2 er målt.** Målingene gjelder WKWebView.                                                                                                                                                                                                                                                                                                                |

---

## 6. Verifiseringssjekkliste for rigg-test

Ingenting under kan kjøres av den automatiske suiten: jsdom har ingen WebGL2,
Playwright kjører headless Chromium, og hele UA-poenget er WebKit-spesifikt.
Kjør i `npm run tauri dev` eller en bygget app, på en ekte video.
Radene E6a–E6g står allerede i `docs/SMOKE-TEST.md`; E8a–E8e er nye.

**Blokkerende (alt annet hviler på disse):**

- [ ] **E6a — UA faktisk anvendt.** Devtools-konsoll → `navigator.userAgent`
      slutter på `Version/17.0 Safari/605.1.15 SundayEdit`. Hvis ikke: wry
      anvendte ikke `tauri.macos.conf.json`, og kompositoren er ~42× tregere
      per frame. **Dette er det eneste beviset for ADR-010s forutsetning.**
- [ ] **E6b — flagg av = ingen endring.** Spill, skrubb og shuttle med GPU-
      previewen AV (standard). Oppfører seg nøyaktig som v0.7.0; ingen canvas i
      DOM, ingen `pixi`-chunk i nettverkspanelet.

**GPU-kompositoren:**

- [ ] **E6c — flagg på, frisk maskin.** Innstillinger → Preview → skru på.
      Bildet spiller videre (nå på canvas), lyden upåvirket, karaoke-captions
      rendres fortsatt OPPÅ, holder 30 fps.
- [ ] **E6d — transform + effekter live.** Skalér/flytt/rotér et klipp og legg
      på brightness/contrast/saturation/svart-hvitt. Previewen viser det
      umiddelbart; sammenlign med en eksport av samme frame — samme retning og
      omtrent samme mengde.
- [ ] **E6e — effekter når eksporten.** Med flagget AV: legg på hver kuratert
      effekt og eksportér. MP4-en viser effekten (eksporten avhang aldri av
      preview-stien).
- [ ] **E6f — fallback er usynlig.** Fremtving en feil (slå av
      maskinvareakselerasjon, eller kjør over en fjernsesjon) med flagget PÅ.
      Faller tilbake til `<video>` uten svart frame og uten krasj;
      Innstillinger viser «utilgjengelig»-notatet og avkrysningsboksen står der
      du lot den.
- [ ] **E6g — veksle midt i økta.** Slå på/av ~10 ganger mens du spiller. Ingen
      lekkede WebGL-kontekster, ingen lydglitch, playhead i sync.

**Nytt fra E8-runden:**

- [ ] **E8a — filmstripen dukker opp uten at du rører noe.** Åpne et prosjekt
      med et video-klipp og LA MASKINEN STÅ. Rammene skal dukke opp av seg selv
      i klippboksen i løpet av et par sekunder — uten at du scroller, zoomer
      eller redigerer. (Dette var funn 3.2; før fiksen krevdes en urelatert
      interaksjon.)
- [ ] **E8b — grov stedfortreder ser riktig ut.** Zoom raskt inn på et langt
      klipp. Mens de fine tiles rendres skal du se et grovere, blassere bilde i
      RIKTIG posisjon — ikke det samme bildet gjentatt og sammenklemt, og ingen
      lys stripe der flere lag ligger oppå hverandre.
- [ ] **E8c — merket for tilnærmet preview.** Med GPU-flagget PÅ: legg en
      beskjæring (crop) på et klipp, og legg deretter to klipp oppå hverandre
      på to videospor. Et lite merke nederst til venstre i previewen sier at
      previewen er omtrentlig, og hva den ikke tegner. Det skal FORSVINNE når
      du fjerner beskjæringen / stabelen.
- [ ] **E8d — karaoke: preview mot innbrenning.** Skru på karaoke, velg
      «sweep», og brenn inn den samme captionen. Steg gjennom en linje frame
      for frame og sammenlign hvilket ord som lyser i preview mot den
      innbrente fila. De skal skifte ord på samme frame — hele veien gjennom
      linja, ikke bare på det første ordet. (Stigen er matematisk pinnet av
      speilparitet-testen; dette verifiserer at libass leser den som forventet.)
- [ ] **E8e — kvalitetsstigen koster ikke eksportkvalitet.** Eksportér mens du
      spiller av, og eksportér parkert. De to filene skal ha identisk
      oppløsning og bitrate — stigen rører bare preview-proxyen.

**Fra tidligere etapper, fortsatt ukjørt:** N1–N7 i `docs/SMOKE-TEST.md`
(import → NLE-backfill, thumbnails, trim/split/move, compose-eksport,
preview-proxy, sporflagg i eksport, remove-guards).

---

## 7. Filer denne runden rørte

| Fil                                                   | Hva                                                              |
| ----------------------------------------------------- | ---------------------------------------------------------------- |
| `src-tauri/tests/mirror_fixture_parity.rs`            | **Ny.** Genererer + verifiserer speilparitet-fixturen.           |
| `src/lib/__fixtures__/mirror-parity.json`             | **Ny, generert.** 64 karaoke- / 138 tile- / 91 effekt-tilfeller. |
| `src/lib/mirrorParity.test.ts`                        | **Ny.** 199 assertions som speiler fixturen i TS.                |
| `src/features/timeline/filmstrip.ts`                  | Funn 3.2 + 3.3 (cache-epoke, stedfortreder-geometri, dedup).     |
| `src/features/timeline/filmstrip.test.ts`             | To nye tester, begge mutasjonsverifisert.                        |
| `src/features/timeline/compositor/scene.ts`           | `approximationNotice` (funn 3.4).                                |
| `src/features/timeline/compositor/scene.test.ts`      | Fem nye tester for merket.                                       |
| `src/features/timeline/compositor/PixiCompositor.tsx` | Merket + isolert canvas-container.                               |
| `src/features/timeline/compositor/index.ts`           | Eksporterer `approximationNotice`.                               |
| `src/lib/i18n.ts`                                     | Tre nye nøkler × 7 språk.                                        |
| `docs/OSS-INTEGRATION-PROGRAM.md`                     | E1–E6 merket ferdig med dato; E7 betinget/utsatt.                |
| `docs/SMOKE-TEST.md`                                  | E8-radene.                                                       |
| `docs/OSS-PROGRAM-REPORT.md`                          | Denne rapporten.                                                 |
