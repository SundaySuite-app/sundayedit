# Releasenotater

Én fil per tagg: `v1.2.3.md`. Fila er teksten operatøren leser i
oppdateringsdialogen inne i appen.

## Hvorfor de bor her

`latest.json` sitt `notes`-felt er det appen viser når den tilbyr en ny versjon.
tauri-action skriver det feltet fra `releaseBody` **mens bygget kjører** — altså
før noen rekker å redigere utgivelsesteksten på GitHub. Så lenge `releaseBody`
var en fast tekst i `release.yml`, fikk hver eneste utgivelse den samme
innholdsløse teksten: v0.5.1 og v0.8.0 bærer begge den samme «See the assets below to download and install. Auto-update is enabled for existing installs.»

Feilen gjorde konkret skade i SundayStage, som flyttet blackout fra Escape til
⇧B i v0.8.0-beta.1 — en vane-endring midt i en gudstjeneste — mens
oppdateringsvarselet sa nøyaktig det samme som forrige gang. Samme
slippmønster, samme felle, i hele suiten.

Når notatet ligger i repoet, finnes teksten før bygget starter, og den blir
lest av noen i samme PR som versjonsbumpen.

## Slik skriver du et

1. Bump versjonen i `package.json` og `src-tauri/tauri.conf.json` (vakten krever at de er like).
2. Lag `docs/release-notes/v<versjon>.md`.
3. `npm run notes:check` — samme vakt som CI kjører.

Skriv til den som faktisk står ved skjermen, ikke til en utvikler. Det viktigste
først: har noe flyttet seg, eller kan noe overraske midt i en økt, skal det stå
i første setning.

## Reglene vakten håndhever

- **Ren tekst.** Ingen overskrifter, fet skrift, tabeller, backticks, lenker
  eller HTML. Dialogen er en liten boks, ikke en markdown-renderer — `**slik**`
  blir stående som stjerner på skjermen.
- **Maks 1000 bytes.** `latest.json` hentes ved hver eneste oppdateringssjekk
  fra hver eneste installasjon, og teksten skal få plass i boksen.
- **Ingen plassholdere.** «TODO», «TBD», fyllmasse eller den gamle engelske
  standardteksten avvises. En fil som finnes men ikke sier noe, gjenskaper
  feilen — bare vanskeligere å oppdage.

Vakten er `tests/integration/releaseNotes.test.ts`, og den kjører på hver PR. Den stopper
altså slippet før noen har begynt å bygge — ikke femten minutter inn i et bygg.

`release.yml` leser den samme fila og avbryter jobben hvis den mangler, slik at
en tagg som er dyttet fra en gammel commit heller ikke slipper unna.

## Den lange versjonen på GitHub-siden

Vil du ha bilder, tabeller og full gjennomgang på selve utgivelsessiden,
rediger utgivelsesteksten på GitHub etter at bygget er ferdig. Det er trygt:
`latest.json` er allerede lastet opp med notatet herfra.
