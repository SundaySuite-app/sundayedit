#!/usr/bin/env node
// Download static ffmpeg + ffprobe into src-tauri/binaries/ with the Rust
// target-triple suffix Tauri's `externalBin` expects (e.g.
// ffmpeg-aarch64-apple-darwin). Run before `tauri build`, both locally and in
// CI (each platform's runner fetches its own binaries).
//
// `--universal` (macOS only) fetches BOTH macOS architectures and lipo's them
// into fat `ffmpeg-universal-apple-darwin` / `ffprobe-universal-apple-darwin`
// sidecars for `tauri build --target universal-apple-darwin`. A universal Tauri
// build needs all three suffixes present: tauri-build validates the per-arch
// sidecar during each cargo slice (`TARGET`), and the bundler picks up the
// `-universal-apple-darwin` one.
//
// ── WHY NOT npm ────────────────────────────────────────────────────────────
// This used to copy the `ffmpeg-static` + `@ffprobe-installer/ffprobe` npm
// binaries. `ffmpeg-static` has been frozen at ffmpeg 6.x (2023) for years, its
// binary is not even in the npm tarball — a postinstall step downloads it from
// GitHub Releases with no integrity check — and @ffprobe-installer was older
// still. This is SundayRec's fetcher (sundayrec/scripts/fetch-ffmpeg.mjs):
// VERSION-PINNED archives straight from the two build servers below, verified
// by SHA-256 before a single byte is unpacked. Same version, same archives,
// same bytes as SundayRec and SundaySync ship.
//
// ── SOURCES (see docs/DISTRIBUTION.md for the licensing note) ──────────────
// macOS + Linux  ffmpeg.martin-riedl.de — release channel, per-binary .zip,
//                signed + notarized on macOS, publishes a .sha256 per archive.
// Windows        gyan.dev release "essentials" build — the long-standing
//                Windows ffmpeg distribution, fetched from its GitHub
//                releases (GyanD/codexffmpeg), which keep every tag; the
//                .sha256 on gyan.dev is where the archive pin comes from.
// Both are GPL builds, same as what we shipped before. `essentials` carries
// everything this app asks ffmpeg for (native aac/flac/pcm/mjpeg, libmp3lame,
// libx264, dshow, lavfi, and every filter we use is built-in).
//
// ── PINNING (two layers) ──────────────────────────────────────────────────
// 1. ARCHIVE: each entry below carries the publisher's SHA-256 for the .zip.
//    A mismatch is a hard failure — a moved or altered download never gets
//    unpacked. These are frozen per FFMPEG_VERSION; bumping the version means
//    re-reading the publisher's .sha256 files.
// 2. BUNDLED BINARY: `scripts/ffmpeg-checksums.json` maps `<name>-<host>` to
//    the hash of the file that actually ships. A pinned entry that mismatches
//    is a hard failure; a MISSING entry logs the computed hash and proceeds,
//    so a platform can be pinned from a trusted run's log (that is how the
//    Windows pins were captured — see the note in that file).
//
// Archives are cached under node_modules/.cache/ so `npm run dev` doesn't
// re-download ~60 MB every launch; an already-correct sidecar short-circuits
// the whole script. Pass --force to re-fetch anyway.

import { execFileSync, spawnSync } from "node:child_process";
import {
  mkdirSync,
  writeFileSync,
  readFileSync,
  existsSync,
  chmodSync,
  rmSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { inflateRawSync } from "node:zlib";
import { fileURLToPath } from "node:url";
import { dirname, join, basename } from "node:path";

// The ffmpeg release this app is built and tested against. 9.0 shipped
// 2026-08-04 and was held back while it was days old — a fresh major is not
// something a Sunday-morning recording should depend on. 9.0.1 (the newest
// 9.0.x BOTH build servers below publish, checked 2026-09-19; 9.0.2 is out on
// ffmpeg.org but not yet built by either) replaced 8.1.2 in the framework
// round of 2026-09-19, gated by CI's real-ffmpeg smokes and a listening check.
const FFMPEG_VERSION = "9.0.1";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const outDir = join(root, "src-tauri", "binaries");
const cacheDir = join(root, "node_modules", ".cache", "sundayedit-ffmpeg");
const force = process.argv.includes("--force");

// ── Download table ─────────────────────────────────────────────────────────

// martin-riedl publishes one .zip per binary, holding a single file named
// `ffmpeg` / `ffprobe` at the archive root. `build` is the immutable
// `<unix-ts>_<version>` directory from the host's release history page.
function martinRiedl(label, build, sha) {
  const entry = (name) => ({
    url: `https://ffmpeg.martin-riedl.de/download/${build}/${name}.zip`,
    sha256: sha[name],
    member: name,
  });
  return {
    label: `ffmpeg.martin-riedl.de ${label} (release ${FFMPEG_VERSION})`,
    binaries: { ffmpeg: entry("ffmpeg"), ffprobe: entry("ffprobe") },
  };
}

// gyan ships both binaries in one archive under `<stem>/bin/<name>.exe`.
//
// The URL is gyan's GITHUB RELEASE, not `gyan.dev/ffmpeg/builds/packages/`.
// That directory holds only the CURRENT release: the day after this pin was
// set, 9.0.2 shipped and the 9.0.1 archive turned into a 404, which broke every
// Windows fetch — CI smoke and release build alike. The GitHub assets are the
// same publisher's same files, kept per tag forever. Nothing else changes: the
// archive SHA-256 below is untouched and GitHub's own digest for the asset
// matches it, so both pin layers still describe the exact same bytes.
// The package directory stays on as a FALLBACK: it still answers for whatever
// is current, which covers a GitHub outage in the window right after a bump.
// Keeping it costs nothing — `archive()` checks every source against the same
// hash, so a fallback can only ever deliver the bytes we already named.
function gyan(stem, sha256) {
  const url = `https://github.com/GyanD/codexffmpeg/releases/download/${FFMPEG_VERSION}/${stem}.zip`;
  const fallbacks = [`https://www.gyan.dev/ffmpeg/builds/packages/${stem}.zip`];
  const entry = (name) => ({
    url,
    fallbacks,
    sha256,
    member: `${stem}/bin/${name}.exe`,
  });
  return {
    label: `gyan.dev ${stem} (release ${FFMPEG_VERSION})`,
    binaries: { ffmpeg: entry("ffmpeg"), ffprobe: entry("ffprobe") },
  };
}

const SOURCES = {
  // Ships in the macOS release (the arm64 half of the universal build).
  "aarch64-apple-darwin": martinRiedl(
    "macOS arm64",
    "macos/arm64/1787073674_9.0.1",
    {
      ffmpeg:
        "8287a1b2229e05eb41859f073e18e6c52c60a778f2f5e6881070fe51b79407fe",
      ffprobe:
        "102a26b8940a053298d9929bfaae71e4b6ef65ba5f19a99a88c433108560741a",
    },
  ),
  // Intel macs — the other half of the universal macOS release (`--universal`).
  "x86_64-apple-darwin": martinRiedl(
    "macOS x86_64",
    "macos/amd64/1787081194_9.0.1",
    {
      ffmpeg:
        "5bdead62ff504ab9b447cc72b212c4fb481e3f7de5877d427a51bee8136dda40",
      ffprobe:
        "34511bbcf1988ad2886023bf5ace4f44cf62e6defeb3d194d6f7619e5b061f7f",
    },
  ),
  // CI only — the ubuntu job needs a real sidecar so the self-skipping
  // real-ffmpeg smokes actually RUN. Nothing is shipped from Linux.
  "x86_64-unknown-linux-gnu": martinRiedl(
    "Linux x86_64",
    "linux/amd64/1787074600_9.0.1",
    {
      ffmpeg:
        "18bec7d5c2ab3b24d277466b758394e109b0479133b98d155c5540ed3013fa74",
      ffprobe:
        "227c122cabb36444d7dee7f5c9c9db9e36e15ab7a9b43eb2196936fb177f9ad3",
    },
  ),
  "aarch64-unknown-linux-gnu": martinRiedl(
    "Linux arm64",
    "linux/arm64/1787072884_9.0.1",
    {
      ffmpeg:
        "92cff3dec20d996bb5b8a918b156b64301338e7c28046053c82d0935cf7c6eb2",
      ffprobe:
        "208379f31219f52333ed769e9159ca2964355b7c7c4420233bcca769b5edef62",
    },
  ),
  // Ships in the Windows release.
  "x86_64-pc-windows-msvc": gyan(
    `ffmpeg-${FFMPEG_VERSION}-essentials_build`,
    "fec81ae03971d9dd4be3ebe02e263bd2ec1d789483f931bdba5f5715e65da2e9",
  ),
};

// ── Host triple ────────────────────────────────────────────────────────────

function hostTriple() {
  let out;
  try {
    out = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  } catch {
    fail(
      "rustc is not on PATH — this script reads the host target triple from " +
        "`rustc -vV`.\n  Install Rust (https://rustup.rs) and re-run.",
    );
  }
  const line = out.split("\n").find((l) => l.startsWith("host:"));
  if (!line)
    fail("`rustc -vV` printed no `host:` line — cannot name the sidecars.");
  return line.slice("host:".length).trim();
}

function fail(msg) {
  console.error(`✗ ${msg}`);
  process.exit(1);
}

const sha256 = (buf) => createHash("sha256").update(buf).digest("hex");

// ── Archive download (cached, integrity-checked) ───────────────────────────

async function archive(url, expected, fallbacks = []) {
  const cached = join(
    cacheDir,
    `${expected.slice(0, 16)}-${basename(new URL(url).pathname)}`,
  );
  if (existsSync(cached) && !force) {
    const buf = readFileSync(cached);
    if (sha256(buf) === expected) {
      console.log(
        `  · cached ${basename(cached)} (${(buf.length / 1e6).toFixed(1)} MB)`,
      );
      return buf;
    }
    rmSync(cached, { force: true });
  }

  // ── Several sources, ONE pin ─────────────────────────────────────
  // Integrity pinning is not AVAILABILITY pinning: the SHA-256 above says what
  // the bytes must be, never that the host still serves them. gyan.dev's
  // package directory keeps only the CURRENT release, so the day after 9.0.1
  // was pinned it turned into a 404 and every Windows build died (2026-09-19).
  // A fallback cannot weaken anything — every source is checked against the
  // same hash, so the worst case is that all of them fail.
  const sources = [url, ...fallbacks];
  const failures = [];
  for (const src of sources) {
    const host = new URL(src).host;
    for (let attempt = 1; attempt <= 3; attempt++) {
      try {
        const res = await fetch(src, {
          redirect: "follow",
          // Generous: these are 30–110 MB archives on a church's connection.
          signal: AbortSignal.timeout(15 * 60 * 1000),
        });
        if (!res.ok) {
          const err = new Error(`HTTP ${res.status} ${res.statusText}`);
          // 404/410 is an answer, not a hiccup: the file is not on this host.
          // Go to the next source at once instead of asking twice more.
          err.gone = res.status === 404 || res.status === 410;
          throw err;
        }
        const buf = Buffer.from(await res.arrayBuffer());
        const actual = sha256(buf);
        if (actual !== expected) {
          throw new Error(
            `SHA-256 mismatch\n    expected ${expected}\n    actual   ${actual}\n` +
              `    (${buf.length} bytes — a truncated download retries, a stable ` +
              `mismatch means the published archive changed)`,
          );
        }
        mkdirSync(cacheDir, { recursive: true });
        writeFileSync(cached, buf);
        console.log(
          `  · downloaded ${(buf.length / 1e6).toFixed(1)} MB from ${host}`,
        );
        return buf;
      } catch (err) {
        failures.push(`${host}: ${err.message}`);
        console.warn(`  ⚠ ${host} attempt ${attempt}/3 failed: ${err.message}`);
        if (err.gone) break;
      }
    }
  }
  fail(
    `could not fetch ${basename(new URL(url).pathname)} from any source:\n` +
      sources.map((s) => `    ${s}`).join("\n") +
      `\n  ${failures.join("\n  ")}\n` +
      `  HTTP 404 everywhere means the publisher pruned this version — bump ` +
      `FFMPEG_VERSION and BOTH pin layers rather than weakening the check.`,
  );
}

// ── Minimal zip reader ─────────────────────────────────────────────────────
// Pure Node (zlib) on purpose: `unzip`, `tar`, `7z` and Expand-Archive are all
// present on SOME of our three runners, none on all three with the same flags.
// The archives are plain 32-bit zips with deflate or stored entries.

const CRC_TABLE = (() => {
  const t = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c;
  }
  return t;
})();

function crc32(buf) {
  let c = -1;
  for (let i = 0; i < buf.length; i++)
    c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}

function zipEntries(zip) {
  // End-of-central-directory lives in the last 64 KB (comment can pad it).
  let eocd = -1;
  for (let i = zip.length - 22; i >= Math.max(0, zip.length - 65557); i--) {
    if (zip.readUInt32LE(i) === 0x06054b50) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0)
    fail("archive is not a zip (no end-of-central-directory record)");

  const count = zip.readUInt16LE(eocd + 10);
  let p = zip.readUInt32LE(eocd + 16);
  if (p === 0xffffffff)
    fail("zip64 archive — this reader only handles 32-bit zips");

  const entries = [];
  for (let i = 0; i < count && zip.readUInt32LE(p) === 0x02014b50; i++) {
    const nameLen = zip.readUInt16LE(p + 28);
    entries.push({
      name: zip.toString("utf8", p + 46, p + 46 + nameLen),
      method: zip.readUInt16LE(p + 10),
      crc: zip.readUInt32LE(p + 16),
      csize: zip.readUInt32LE(p + 20),
      usize: zip.readUInt32LE(p + 24),
      offset: zip.readUInt32LE(p + 42),
    });
    p += 46 + nameLen + zip.readUInt16LE(p + 30) + zip.readUInt16LE(p + 32);
  }
  return entries;
}

function extract(zip, member) {
  const entries = zipEntries(zip);
  const leaf = member.split("/").pop();
  const hits = entries.filter(
    (e) => e.name === member || e.name.endsWith(`/${leaf}`),
  );
  if (hits.length !== 1) {
    fail(
      `expected exactly one \`${member}\` in the archive, found ${hits.length}.\n` +
        `  Archive holds: ${entries
          .map((e) => e.name)
          .slice(0, 12)
          .join(", ")}${entries.length > 12 ? ", …" : ""}`,
    );
  }
  const e = hits[0];
  if (e.offset === 0xffffffff || e.csize === 0xffffffff) {
    fail(`\`${e.name}\` uses zip64 fields — unsupported`);
  }
  if (zip.readUInt32LE(e.offset) !== 0x04034b50) {
    fail(`\`${e.name}\` has a corrupt local header`);
  }
  const start =
    e.offset +
    30 +
    zip.readUInt16LE(e.offset + 26) +
    zip.readUInt16LE(e.offset + 28);
  const raw = zip.subarray(start, start + e.csize);

  let out;
  if (e.method === 0) out = Buffer.from(raw);
  else if (e.method === 8)
    out = inflateRawSync(raw, { maxOutputLength: e.usize });
  else fail(`\`${e.name}\` uses unsupported compression method ${e.method}`);

  if (out.length !== e.usize) {
    fail(`\`${e.name}\` unpacked to ${out.length} bytes, expected ${e.usize}`);
  }
  if (crc32(out) !== e.crc) fail(`\`${e.name}\` failed its CRC-32 check`);
  return out;
}

// ── Verification ───────────────────────────────────────────────────────────

// `<bin> -version` exits 0 and names itself AND its version — the cheapest
// proof the file is a real, runnable executable for this arch (not a truncated
// download or an HTML error page saved under the binary's name) AND that it is
// the ffmpeg the Rust arg builders and output parsers were tested against.
// spawnSync never throws and runs no shell, so paths with spaces are safe.
function probe(bin, name) {
  if (!existsSync(bin)) return null;
  const r = spawnSync(bin, ["-version"], { encoding: "utf8", timeout: 60_000 });
  if (r.error || r.status !== 0) return null;
  const m = `${r.stdout}${r.stderr}`.match(/^(ffmpeg|ffprobe) version (\S+)/m);
  if (!m || m[1] !== name) return null;
  // BtbN-style builds prefix the tag with `n`; martin-riedl/gyan don't.
  const version = m[2].replace(/^n/, "");
  return version.startsWith(FFMPEG_VERSION) ? version : null;
}

const checksums = JSON.parse(
  readFileSync(join(root, "scripts", "ffmpeg-checksums.json"), "utf8"),
);

// Returns true when the bytes are pinned and match (or unpinned — logged).
function checkPin(name, host, buf) {
  const key = `${name}-${host}`;
  const actual = sha256(buf);
  const expected = checksums[key];
  if (expected && actual !== expected) {
    fail(
      `${name}: SHA-256 mismatch for ${key}\n` +
        `  expected ${expected}\n` +
        `  actual   ${actual}\n` +
        `  The unpacked binary is not the pinned one — refusing to bundle it.`,
    );
  }
  if (expected) {
    console.log(`  ✓ ${name}: SHA-256 verified (${key})`);
  } else {
    console.warn(
      `  ⚠ ${name}: no pinned SHA-256 for ${key} — computed ${actual}\n` +
        `    Pin it by adding "${key}": "${actual}" to scripts/ffmpeg-checksums.json`,
    );
  }
  return actual;
}

// ── Main ───────────────────────────────────────────────────────────────────

const universal = process.argv.includes("--universal");
const host = hostTriple();
const ext = host.includes("windows") ? ".exe" : "";
if (universal && !host.endsWith("-apple-darwin")) {
  fail("--universal is only supported on macOS hosts.");
}

// The host always; with --universal, both macOS halves (host first).
const targets = universal
  ? [
      host,
      ...["aarch64-apple-darwin", "x86_64-apple-darwin"].filter(
        (t) => t !== host,
      ),
    ]
  : [host];

mkdirSync(outDir, { recursive: true });

// `lipo -archs` names the arm64 slice `arm64`, not `aarch64`.
const lipoArch = (triple) =>
  triple.startsWith("aarch64") ? "arm64" : triple.split("-")[0];

// A non-host macOS slice cannot be RUN here (no Rosetta on a runner is
// guaranteed), so it is proven by its pinned hash plus its Mach-O arch.
function sliceIs(bin, triple) {
  const r = spawnSync("lipo", ["-archs", bin], { encoding: "utf8" });
  return r.status === 0 && r.stdout.trim() === lipoArch(triple);
}

async function fetchFor(target) {
  const source = SOURCES[target];
  if (!source) {
    fail(
      `no ffmpeg ${FFMPEG_VERSION} download is configured for \`${target}\`.\n` +
        `  Known hosts: ${Object.keys(SOURCES).join(", ")}\n` +
        `  Add one to the SOURCES table in scripts/fetch-ffmpeg.mjs.`,
    );
  }
  const runnable = target === host;
  const suffix = target === host ? ext : "";
  console.log(`ffmpeg ${FFMPEG_VERSION} for ${target} — ${source.label}`);

  for (const [name, spec] of Object.entries(source.binaries)) {
    const dest = join(outDir, `${name}-${target}${suffix}`);
    const pin = checksums[`${name}-${target}`];

    // Fast path: the right binary is already there. `npm run dev` runs this
    // script on every launch — it must not re-download 60 MB each time.
    if (!force && existsSync(dest)) {
      const pinned = !pin || sha256(readFileSync(dest)) === pin;
      const have = runnable
        ? probe(dest, name)
        : sliceIs(dest, target) && pin && FFMPEG_VERSION;
      if (have && pinned) {
        console.log(`✓ ${name} ${have} already in place (${target})`);
        continue;
      }
    }

    console.log(`→ ${name}`);
    const zip = await archive(spec.url, spec.sha256, spec.fallbacks);
    const bin = extract(zip, spec.member);
    checkPin(name, target, bin);
    if (!runnable && !pin) {
      fail(
        `${name}: ${target} cannot be run on this host, so it must be pinned in ` +
          `scripts/ffmpeg-checksums.json before it can be bundled.`,
      );
    }

    writeFileSync(dest, bin);
    chmodSync(dest, 0o755);

    const version = runnable
      ? probe(dest, name)
      : sliceIs(dest, target) && FFMPEG_VERSION;
    if (!version) {
      rmSync(dest, { force: true });
      fail(
        `${name}: the unpacked binary ${runnable ? "does not run, or is not ffmpeg " + FFMPEG_VERSION : "is not a " + lipoArch(target) + " Mach-O"}.\n` +
          `  Source: ${spec.url}\n` +
          `  (removed it — a broken sidecar must never reach a build)`,
      );
    }
    console.log(
      `✓ ${name} ${version} → src-tauri/binaries/${name}-${target}${suffix} ` +
        `(${(bin.length / 1e6).toFixed(1)} MB, ${runnable ? "runs" : "pinned " + lipoArch(target) + " slice"})`,
    );
  }
}

for (const target of targets) await fetchFor(target);

// ── Universal macOS sidecars ───────────────────────────────────────────────

if (universal) {
  for (const name of ["ffmpeg", "ffprobe"]) {
    const out = join(outDir, `${name}-universal-apple-darwin`);
    execFileSync("lipo", [
      "-create",
      "-output",
      out,
      join(outDir, `${name}-aarch64-apple-darwin`),
      join(outDir, `${name}-x86_64-apple-darwin`),
    ]);
    chmodSync(out, 0o755);
    const archs = execFileSync("lipo", ["-archs", out], {
      encoding: "utf8",
    }).trim();
    if (!archs.includes("x86_64") || !archs.includes("arm64")) {
      fail(`${out} is not universal (archs: ${archs})`);
    }
    console.log(
      `✓ ${name} → src-tauri/binaries/${name}-universal-apple-darwin (${archs})`,
    );
  }
}
