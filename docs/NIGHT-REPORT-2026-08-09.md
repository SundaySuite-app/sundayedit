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

## ✅ Morgenbeslutningene — gjennomført 2026-08-10

Alle sju punktene under er utført; teksten står igjen som historikk.
Sluttilstand: **0 åpne PR-er, 0 ekstragrener, `npm audit` 0, dependabot
17 → 1 varsel** (`glib`, medium, ingen in-range-fiks — vent på oppstrøms).

1–3. **Alle dependency-PR-ene** ble samlet i #43 i stedet for å merges hver
for seg: de ugyldiggjorde hverandres lockfiler, og #36 hadde rukket å
få konflikt. vite 8 + plugin-react 6 + jest-dom 7 + `npm audit fix` +
`serde_with` landet som én verifisert enhet. #26 (actions) og #33 (zip 8)
merget separat. #37/#36/#31/#30/#29 lukket som superseded. 4. **#32 sunday-auth** løste seg ved etterprøving: Rec/Stage/Paper kjørte
allerede v0.4.1, så Edit var etternøleren. Dependabots gren var bygget på
en juli-main (580 tester mot dagens 740), så dens grønne CI beviste
ingenting — endringen ble verifisert på ekte main og merget der (#45). 5. **Grenene** er ryddet; kun `main` står igjen. 6. **Apple-avtalen ble omgått, ikke løst** (eierordre: «dropp den om du kan»).
`release.yml` notariserer nå kun ved tag-push; manuell `workflow_dispatch`
er escape-luken som gir et signert, u-notarisert bygg. Notarisering slår
seg derfor på av seg selv den dagen avtalen signeres — ingen bryter å
huske. **v0.8.0 er bygget, publisert og satt som Latest.**
To feller verdt å kjenne, begge funnet her:

- `APPLE_ID: ''` slår **ikke** av notarisering. En tom-men-definert
  variabel leses som `Ok("")`, så bundleren går inn i notariseringsstien
  og dør på «Team ID must be at least 3 characters» — etter et rent bygg
  og vellykket signering. Variablene må ikke _eksistere_.
- En `type: boolean` workflow-input kan ikke sammenlignes med en literal:
  GitHub caster begge til tall, så både `"true"` og `"false"` blir NaN og
  ingen er lik `true`. En slik bryter betyr stille sin default for alltid.

7. **Rigg-testen gjenstår** — den eneste flaten som krever eier + ekte video.

### Artefakt-verifisering (v0.8.0, etterprøvd på nedlastet DMG)

- Hovedbinær **og** begge ffmpeg-sidecars er ekte universal (`x86_64 arm64`).
- Signaturen er gyldig og tilfredsstiller sin Designated Requirement;
  utstedt til _Developer ID Application: Richard Fossland (784GN847G4)_,
  full kjede til Apple Root CA, sikkert tidsstempel, hardened runtime på.
- Gatekeeper avviser med nøyaktig én grunn: `Unnotarized Developer ID` —
  altså er bygget klart for notarisering i det avtalen er på plass.
- Alle åtte updater-signaturer bærer nøkkel-ID `62b77f9fd487be44`, identisk
  med pubkey-en appen har bakt inn → auto-oppdatering verifiserer.

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
