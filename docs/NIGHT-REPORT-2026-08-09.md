# Nattrapport 2026-08-08 → 09

Autonom nattrunde etter eier-godkjent plan (opprydding, utsatt arbeid, full
kodegjennomgang). Avbrutt av credit-tak midt i N2, gjenopptatt og fullført
09-08. Alt på main er full-gate-verifisert: `npm run check` (vitest 318,
cargo 635, clippy `-D warnings`, eslint), `vite build`, Playwright 49/49,
og alle 18 ekte-ffmpeg-integrasjonstester.

## Landet på main (PR #35 + N1-merger)

### N2 — Feiljakt: 22 adversarielt bekreftede funn, alle fikset (`d54acac`)

33 agenter (4 finnere med ulike linser + 22 verifikatorer + fiksere), hvert
funn repro-bevist FØR fiks, hver fiks med regresjonstest. Utvalg:

| Funn                              | Konsekvens før fiks                                                                                                                                                                                                          |
| --------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Import-gap                        | Fersk import ga tomt Medier-panel/tomme spor (backfill fantes kun ved load, plasserte aldri klipp). Nå: felles `backfill_default_timeline` ved create+load, m/ `tracks_persisted`-markør så tømt tidslinje ikke gjenoppstår. |
| Eksport ignorerte spor-flagg      | Deaktiverte/mutede/solo-spor rendret og lød likevel i eksport.                                                                                                                                                               |
| Simple-path uten fremdrift/avbryt | Eksportmodal hang på 0 % og Cancel virket ikke på hurtigstien.                                                                                                                                                               |
| Latchet «Video utilgjengelig»     | Overlayet satt seg permanent etter én feil (samme symptom som juli-buggen).                                                                                                                                                  |
| Ugyldige xfade-navn i UI          | «crossfade»/«dip» finnes ikke i ffmpeg → eksport feilet ved render. Nå kun ekte navn + legacy-normalisering, bevist per navn mot ekte ffmpeg.                                                                                |
| CAS-commit + kø i store           | Samtidig setProject under async op ble klobret; raske slider-commits ble stille droppet.                                                                                                                                     |
| f32-presisjon                     | `timeline_end_ms` regnes nå i f64, paritet med TS-speilet testes via JSON-round-trip.                                                                                                                                        |
| Trim/split-clamper                | Naboklipp-clamp uten innholdsglidning; slowmo-split taper ikke frames; overlappende add plasseres i gapet i stedet for stille avvisning.                                                                                     |

Merk: mistanken om speed-asymmetri i trim var **avkreftet** (koden var
korrekt) — men jakten fant to reelle kantfeil i samme område i stedet.

### N3+N4 — Utsatt arbeid + effektivitet (`0d0036e`)

- **Thumbnails ende-til-ende** (MediaBin + klippbokser, cache per media-id).
- **Split (B)**, slett klipp (Delete), fjern spor/media m/ feilvisning.
- **i18n:** NLE-blokka oversatt for sv/da/de/fr/pl (var rå engelsk).
- **Render-effektivitet:** Timeline re-rendret hele skogen ~60×/s under
  avspilling; nå kun timecode + spillehodelinje + MediaPlayer (memoiserte
  RulerBar/LaneHeaders/LaneStack). ClipInspector leser playhead via egen
  liten store — App re-rendrer ikke per frame.
- **Ytelsesvakt:** 5000-klipps stresstest; `validate_timeline` målt 1,35 ms.
- Vitest 283→318, Playwright 45→49. Bonus: e2e kjørte mot foreldet `dist/`
  (CI var OK, kun lokalt) — spec fikset.

### N5 — Dokumentasjon + hygiene (`2126302`)

README totalskrevet (påsto «Video import … pending»!), ARCHITECTURE/
DECISIONS/SMOKE-TEST/NEEDS-RICHARD àjour (bl.a. fjernet usann «no in-app
Transcribe action»), ny `CHANGELOG.md`, siste i18n-nøkler, tomme kataloger
slettet, `.DS_Store`/`test-results` ryddet, lokal gren `feat/nle-multitrack`
slettet (ren forfar av main).

### N1 — Avhengigheter (merget i natt)

npm-gruppen (23 oppdateringer), cargo-gruppen (9), actions-gruppen,
`quinn-proto` (RUSTSEC high). Lukket: fast-uri, js-yaml, postcss m.fl.

## 👤 Morgenbeslutninger (alt klart, én handling per punkt)

1. **Merge PR #37** — lockfile-only sikkerhets-bumper (undici×11,
   brace-expansion, esbuild, serde_with). Full gate grønn. `npm audit` = 0.
2. **Merge PR #36** — vite 8 + plugin-react 6 som atomisk par (dependabot
   #30/#31 er et låst peer-par, umulige enkeltvis — kombinasjonen er
   full-gate-verifisert). Lukker high-varselet på vite. **Lukk #30 + #31
   manuelt etterpå.**
3. **Merge PR #29** (jest-dom 7) og **PR #33** (zip 8) — begge verifisert
   grønne lokalt mot main (kommentarer ligger på PR-ene).
   → Etter 1–4 gjenstår ett varsel: `glib` medium (transitiv via tauri,
   ingen in-range-fiks — vent på oppstrøms).
4. **PR #32 (sunday-auth v0.4.1)** — IKKE rørt: kryss-repo SSO-kontrakt.
   Sjekk sunday-platform-CHANGELOG før beslutning.
5. **Remote-grener** (klassifisereren stoppet autonom sletting — korrekt).
   Innholdet er i main via squash-merger; slett når du vil:
   ```
   git push origin --delete feat/adopt-contracts-mediahandoff feat/highlight-reel-studio fix/whisper-metal-progress-cancel fix/reel-render-spawn-blocking feat/universal-macos-target
   ```
   Lokale rester: `git branch -D feat/adopt-contracts-mediahandoff feat/highlight-reel-studio fix/whisper-metal-progress-cancel`
6. **v0.7.0-utgivelsen (fra juli):** draften har kun Windows-installer.
   macOS-DMG-en blokkeres fortsatt av Apple-avtalen → godta på
   developer.apple.com (Membership → Agreements), re-kjør feilet jobb
   (`gh run rerun 29450117806 --failed`), publiser draften.
7. **Rigg-test:** `docs/SMOKE-TEST.md` har nye NLE-rader N1–N7 (ekte video
   → import → klipp → eksport). Eneste flate natta ikke kan verifisere.

## Flagget, ikke rørt (bevisste beslutninger for eier)

- **Highlight-Reel-backend** uten UI (PR #11): bygg UI eller fjern.
- **Sunday Account-kommandoer** uten UI (SSO-plan?).
- **`site/`** (landingsside) uten deploy-wiring.
- **`ipc.project.relink`** uten relink-UI; **`addTextItem`** uten UI.
- **WebCodecs sanntids-kompositor** — fortsatt bevisst utsatt (ADR-009);
  preview-proxy dekker behovet.

## Prosessnotater

- Sikkerhetsklassifisereren blokkerte tre autonome handlinger (remote-gren-
  sletting, majors-merge, sen lockfile-merge) — alle omgjort til
  verifiser-og-rapportér + eier-beslutning. Ingen forsøk på omgåelse.
- Nattrunden brukte ~4,9 M agent-tokens over 6 workflows / 47 agenter.
- Lærdom herdet i suiten: kjør `npm run build` før lokal Playwright
  (e2e server dist/); grupperte dependabot-majors kan være låste peer-par.
