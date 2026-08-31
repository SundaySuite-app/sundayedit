/**
 * Vakten som gjør at et slipp ikke kan kuttes uten et releasenotat.
 *
 * `latest.json` sitt `notes`-felt er teksten brukeren leser i
 * oppdateringsbanneret, og tauri-action skriver det feltet ved BYGGETID. En
 * tekst som skrives på GitHub etterpå når aldri manifestet — og dermed aldri
 * brukeren. v0.5.1 og v0.8.0 bærer begge den samme «See the assets below to
 * download and install. Auto-update is enabled for existing installs.»
 *
 * Vakten ligger her, i `web`-jobben på PR, og ikke i `release.yml`, fordi det
 * er her noen kan gjøre noe med den. Feiler den i et bygg, er slippet allerede
 * i gang og halve poenget tapt. (`release.yml` sjekker det samme én gang til,
 * for tagger dyttet fra en commit som er eldre enn denne testen.)
 *
 * Den viktigste egenskapen: en manglende notatfil faller IKKE tilbake til en
 * standardtekst. Da hadde vi hatt samme feil, bare skjult bak en grønn CI.
 */
import { describe, expect, it } from "vitest";

import {
  MAX_NOTE_BYTES,
  byteLength,
  checkAll,
  checkNote,
  knownTags,
  normalize,
  packageVersion,
  tauriVersion,
} from "../../scripts/release-notes.mjs";

describe("releasenotat", () => {
  it("finnes for versjonen som bygges, og holder alle reglene", () => {
    const result = checkAll();
    // Problemene skrives ut i sin helhet: den som blir stoppet skal kunne
    // rette hele fila i én runde, uten å måtte gjette hva som var galt.
    expect(result.problems).toEqual([]);
    expect(result.ok).toBe(true);
  });

  it("gjelder taggen som faktisk bygges — versjonene må være i takt", () => {
    // Tauri stempler `tauri.conf.json`-versjonen inn i `latest.json`. Spriker
    // de to, kan vi ha godkjent notatet for en annen versjon enn den som
    // havner i manifestet.
    expect(tauriVersion()).toBe(packageVersion());
  });

  it("er kort nok til å bo i et manifest som hentes ved hver sjekk", () => {
    for (const tag of knownTags()) {
      const note = checkNote(tag);
      expect(
        note.ok,
        note.ok ? tag : `${tag}: ${note.problems.join("; ")}`,
      ).toBe(true);
      expect(byteLength(note.text)).toBeLessThanOrEqual(MAX_NOTE_BYTES);
    }
  });

  it("sier fra med filstien når notatet mangler, i stedet for å finne på en tekst", () => {
    const result = checkNote("v0.0.0-finnes-ikke");

    expect(result.ok).toBe(false);
    expect(result.problems.join(" ")).toContain(
      "docs/release-notes/v0.0.0-finnes-ikke.md",
    );
    // Ingen `text` å falle tilbake på. Det er hele forskjellen fra før.
    expect(result.text).toBeUndefined();
  });
  it("leser notatet likt enten det er sjekket ut med LF eller CRLF", () => {
    // Release-jobben kjører på macOS OG Windows, og ingen av repoene
    // normaliserer linjeskift i git. Uten dette avgjorde kappløpet mellom de to
    // runnerne hva som havnet i `latest.json`.
    expect(
      normalize("Blackout er \u21e7B.\r\n\r\nEscape lukker biblioteket.\r\n"),
    ).toBe("Blackout er \u21e7B.\n\nEscape lukker biblioteket.\n");
  });
});
