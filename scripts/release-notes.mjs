#!/usr/bin/env node
/**
 * Releasenotatet som finnes FØR bygget.
 *
 * ## Hvorfor denne fila finnes
 *
 * `latest.json` sitt `notes`-felt er teksten operatøren leser i
 * oppdateringsdialogen. tauri-action skriver det feltet fra `releaseBody` mens
 * bygget kjører, altså før noen rekker å redigere utgivelsesteksten på GitHub.
 * Så lenge `releaseBody` var en fast tekst i `release.yml`, var notatet
 * garantert innholdsløst: v0.5.1 og v0.8.0 bærer begge den samme «See the assets below to download and install. Auto-update is enabled for existing installs.»
 *
 * Feilen gjorde konkret skade i SundayStage, som flyttet blackout fra Escape
 * til ⇧B i v0.8.0-beta.1 — en vane-endring midt i en gudstjeneste — mens
 * oppdateringsvarselet sa nøyaktig det samme som forrige gang. Samme
 * slippmønster, samme felle, i alle søsterappene.
 *
 * Notatet bor derfor i repoet, én fil per tagg, og blir reviewet i samme PR som
 * versjonsbumpen. Da finnes teksten før bygget starter, og den kan ikke bli
 * glemt.
 *
 * ## Reglene, og hvorfor de er der
 *
 * - **Ren tekst, ikke markdown.** Notatet vises i en liten boks i appen, ikke i
 *   en markdown-renderer. `**fet**` og tabellrader blir stående som tegn på
 *   skjermen. Regelen tvinger fram den skrivemåten dialogen fortjener uansett:
 *   korte setninger til en frivillig som står ved skjermen.
 * - **Lengdegrense.** `latest.json` hentes ved hver eneste oppdateringssjekk fra
 *   hver eneste installasjon. Et notat på flere kilobyte blåser opp manifestet,
 *   og det får uansett ikke plass i boksen.
 * - **Ingen plassholdere.** En fil som finnes, men fortsatt sier «TODO», er
 *   verre enn ingen fil: vakten blir grønn og feilen blir usynlig igjen.
 *
 * Vil du ha den lange versjonen på GitHub-siden, rediger utgivelsesteksten der
 * etterpå. Det er trygt: `latest.json` er allerede lastet opp.
 *
 * ## Bruk
 *
 *   node scripts/release-notes.mjs --check         # vakten, kjørbar for hånd
 *   node scripts/release-notes.mjs --emit v1.2.3   # skriver GITHUB_OUTPUT-blokk
 *
 * `tests/integration/releaseNotes.test.ts` er den samme vakten som test, og
 * det er den som faktisk stopper en PR.
 */
import { randomUUID } from "node:crypto";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));

/** Rota i repoet, uavhengig av hvor kommandoen kjøres fra. */
export const REPO_ROOT = resolve(HERE, "..");

/** Der notatene bor. Én fil per tagg: `v1.2.3.md`. */
export const NOTES_DIR = join(REPO_ROOT, "docs", "release-notes");

/**
 * Taket for ett notat, i BYTES — ikke tegn. «», ⇧ og ⌘ er flere bytes hver, og
 * det er bytes `latest.json` bærer over nettet.
 *
 * 1000 bytes er rundt 12 linjer i dialogboksen. Rikelig til «dette er nytt, og
 * dette må du vite før søndag», altfor lite til en utviklerlogg — som er hele
 * poenget med et tak.
 */
export const MAX_NOTE_BYTES = 1000;

/** Det minste som kan kalles et notat. Under dette er det en plassholder. */
export const MIN_NOTE_BYTES = 40;

/** Filnavnet for én tagg. */
export const notePath = (tag) => join(NOTES_DIR, `${tag}.md`);

/** Taggen for én versjon. Taggene i dette repoet er `v` + versjonen. */
export const tagFor = (version) => `v${version}`;

/**
 * Samme notat, uansett hvilken plattform som leser det.
 *
 * Ingen av suitens repoer normaliserer linjeskift i git, så en Windows-runner
 * sjekker ut `.md` med CRLF mens macOS får LF. Release-jobben kjører på BEGGE,
 * og begge sender teksten inn i den samme utgivelsen — uten dette hadde
 * innholdet i `latest.json` avhengt av hvilken av dem som rakk fram først, og
 * byte-taket under hadde talt ett tegn ekstra per linje på den ene.
 */
export const normalize = (text) => text.replace(/\r\n/g, "\n");

/** Bytes, ikke `String.length` — se `MAX_NOTE_BYTES`. */
export const byteLength = (text) => Buffer.byteLength(text, "utf8");

const readJson = (path) =>
  JSON.parse(readFileSync(join(REPO_ROOT, path), "utf8"));

/** Versjonen appen bygges som. */
export function packageVersion() {
  return readJson("package.json").version;
}

/** Versjonen Tauri stempler inn i bundlen og i `latest.json`. */
export function tauriVersion() {
  return readJson("src-tauri/tauri.conf.json").version;
}

/** Alle taggene det finnes notat for. */
export function knownTags() {
  if (!existsSync(NOTES_DIR)) return [];
  return readdirSync(NOTES_DIR)
    .filter((name) => name.startsWith("v") && name.endsWith(".md"))
    .map((name) => name.slice(0, -".md".length))
    .sort();
}

/**
 * Markdown som blir til støy når teksten vises som ren tekst i en liten boks.
 * Hver regel navngir det den forbyr, fordi meldingen er halve vakten: den som
 * blir stoppet skal vite hva de skal skrive i stedet.
 */
const PLAIN_TEXT_RULES = [
  [/^\s*#/m, "overskrifter (#) — dialogen har ingen overskriftsstil"],
  [/\*\*|__/, "fet skrift (** eller __) — stjernene blir stående på skjermen"],
  [/^\s*\|/m, "tabeller (|) — de blir én lang linje i boksen"],
  [/`/, "backticks — de blir stående som tegn"],
  [/\]\(/, "lenker — operatøren kan ikke klikke i denne boksen"],
  [/<[a-z!/]/i, "HTML og HTML-kommentarer"],
];

/**
 * Tekst som betyr «noen rakk ikke å skrive dette». Med i vakten fordi en
 * plassholder som slipper gjennom gjenskaper nøyaktig feilen dette fikser,
 * bare vanskeligere å få øye på.
 */
const PLACEHOLDER_PATTERNS = [
  [/\bTODO\b|\bTBD\b|\bFIXME\b|\bXXX\b/i, "en TODO/TBD-markør"],
  [/see the assets below/i, "den gamle engelske standardteksten"],
  [/lorem ipsum/i, "fyllmasse"],
  [/\.\.\.$|…$/m, "en linje som slutter i løse luften"],
];

/**
 * Les og godkjenn ett notat.
 *
 * Returnerer alle problemene, ikke bare det første: den som bumper versjonen
 * skal kunne rette hele fila i én runde.
 */
export function checkNote(tag) {
  const path = notePath(tag);
  const relative = path.slice(REPO_ROOT.length + 1);

  if (!existsSync(path)) {
    return {
      ok: false,
      tag,
      path,
      problems: [
        `mangler ${relative} — skriv notatet for ${tag} der (se docs/release-notes/README.md)`,
      ],
    };
  }

  const text = normalize(readFileSync(path, "utf8")).trim();
  const problems = [];
  const bytes = byteLength(text);

  if (bytes < MIN_NOTE_BYTES) {
    problems.push(
      `${relative} er tom eller nesten tom (${bytes} bytes) — den blir teksten operatøren leser`,
    );
  }
  if (bytes > MAX_NOTE_BYTES) {
    problems.push(
      `${relative} er ${bytes} bytes, taket er ${MAX_NOTE_BYTES} — latest.json hentes ved hver oppdateringssjekk, og teksten skal få plass i dialogboksen`,
    );
  }
  for (const [pattern, what] of PLACEHOLDER_PATTERNS) {
    if (pattern.test(text)) problems.push(`${relative} inneholder ${what}`);
  }
  for (const [pattern, what] of PLAIN_TEXT_RULES) {
    if (pattern.test(text)) {
      problems.push(
        `${relative}: notatet vises som ren tekst, så ikke bruk ${what}`,
      );
    }
  }

  return problems.length
    ? { ok: false, tag, path, problems }
    : { ok: true, tag, path, text };
}

/**
 * Hele vakten: versjonen som bygges har sitt notat, og ingen av de andre
 * notatene har råtnet.
 */
export function checkAll() {
  const problems = [];

  // Tauri stempler `tauri.conf.json`-versjonen inn i `latest.json`. Spriker de
  // to, kan vi ha godkjent notatet for en annen versjon enn den som havner i
  // manifestet.
  const tauri = tauriVersion();
  if (packageVersion() !== tauri) {
    problems.push(
      `package.json er ${packageVersion()} og src-tauri/tauri.conf.json er ${tauri} — taggen bygget lages fra må være den notatet er skrevet for`,
    );
  }

  const pkg = packageVersion();
  const tags = new Set([...knownTags(), tagFor(pkg)]);
  for (const tag of [...tags].sort()) {
    const result = checkNote(tag);
    if (!result.ok) problems.push(...result.problems);
  }

  return {
    ok: problems.length === 0,
    problems,
    version: pkg,
    tag: tagFor(pkg),
  };
}

// ── CLI ─────────────────────────────────────────────────────────────────────

function fail(lines) {
  for (const line of lines) console.error(`  ✗ ${line}`);
  process.exit(1);
}

function main(argv) {
  const [mode, arg] = argv;

  if (mode === "--check") {
    const result = checkAll();
    if (!result.ok) {
      console.error("Releasenotatet holder ikke:");
      fail(result.problems);
    }
    console.log(
      `✓ ${result.tag} har notat, og ${knownTags().length} notat(er) er gyldige.`,
    );
    return;
  }

  if (mode === "--emit") {
    const tag = arg ?? tagFor(packageVersion());
    const result = checkNote(tag);
    if (!result.ok) {
      console.error(`Kan ikke slippe ${tag} uten et gyldig releasenotat:`);
      fail(result.problems);
    }
    // Tilfeldig skilletegn: notatet er tekst fra repoet, og et fast skilletegn
    // ville vært noe en notatfil kunne inneholde og dermed bryte ut av.
    const delimiter = `SUNDAY_NOTES_${randomUUID().replaceAll("-", "")}`;
    process.stdout.write(`notes<<${delimiter}\n${result.text}\n${delimiter}\n`);
    return;
  }

  console.error("bruk: release-notes.mjs --check | --emit [tagg]");
  process.exit(2);
}

// Kjør bare som kommando, ikke når testene importerer modulen.
if (
  process.argv[1] &&
  resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))
) {
  main(process.argv.slice(2));
}
