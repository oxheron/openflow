#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  readdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(scriptDirectory, "..");
const outputPath = join(repositoryRoot, "packaging", "THIRD_PARTY_LICENSES.txt");
const mode = process.argv[2] ?? "--check";

if (!["--write", "--check", "--check-inputs"].includes(mode) || process.argv.length > 3) {
  throw new Error(
    "usage: scripts/generate-third-party-licenses.mjs [--write|--check|--check-inputs]",
  );
}

const inputPaths = {
  cargo: join(repositoryRoot, "Cargo.lock"),
  npm: join(repositoryRoot, "apps", "desktop", "package-lock.json"),
  native: join(repositoryRoot, "packaging", "upstream.env"),
};

function sha256(contents) {
  return createHash("sha256").update(contents).digest("hex");
}

function inputDigests() {
  return Object.fromEntries(
    Object.entries(inputPaths).map(([name, path]) => [name, sha256(readFileSync(path))]),
  );
}

function expectedHeader(digests) {
  return [
    "OpenFlow third-party dependency licenses",
    "",
    "This file is generated deterministically. Do not edit it by hand.",
    `Cargo.lock SHA-256: ${digests.cargo}`,
    `package-lock.json SHA-256: ${digests.npm}`,
    `packaging/upstream.env SHA-256: ${digests.native}`,
  ].join("\n");
}

function checkInputs() {
  if (!existsSync(outputPath)) {
    throw new Error(`${relative(repositoryRoot, outputPath)} is missing; generate and review it`);
  }
  const header = expectedHeader(inputDigests());
  if (!readFileSync(outputPath, "utf8").startsWith(`${header}\n`)) {
    throw new Error(
      "dependency inputs changed without regenerated license notices; run " +
        "scripts/generate-third-party-licenses.mjs --write and review the result",
    );
  }
}

if (mode === "--check-inputs") {
  checkInputs();
  process.stdout.write("Third-party license input digests are current\n");
  process.exit(0);
}

const licenseName = /^(license|licence|copying|copyright|notice)([-_.].*)?$/i;

function licenseFiles(directory, explicitFile) {
  const paths = [];
  if (explicitFile) {
    const path = resolve(directory, explicitFile);
    if (!existsSync(path) || !statSync(path).isFile()) {
      throw new Error(`declared license file is missing: ${path}`);
    }
    paths.push(path);
  }
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.isFile() && licenseName.test(entry.name)) paths.push(join(directory, entry.name));
  }
  return [...new Set(paths)].sort((left, right) => left.localeCompare(right));
}

function normalizeText(contents) {
  return contents.replaceAll("\r\n", "\n").replaceAll("\r", "\n").trimEnd() + "\n";
}

const mitLicense = `Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.`;

const bsdThreeClauseLicense = `Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice,
   this list of conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.
3. Neither the name of the copyright holder nor the names of its contributors
   may be used to endorse or promote products derived from this software
   without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.`;

const boostLicense = `Boost Software License - Version 1.0 - August 17th, 2003

Permission is hereby granted, free of charge, to any person or organization
obtaining a copy of the software and accompanying documentation covered by
this license (the "Software") to use, reproduce, display, distribute, execute,
and transmit the Software, and to prepare derivative works of the Software,
and to permit third-parties to whom the Software is furnished to do so, all
subject to the following:

The copyright notices in the Software and this entire statement, including the
above license grant, this restriction and the following disclaimer, must be
included in all copies of the Software, in whole or in part, and all derivative
works of the Software, unless such copies or derivative works are solely in the
form of machine-executable object code generated by a source language
processor.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE, TITLE AND NON-INFRINGEMENT. IN NO EVENT
SHALL THE COPYRIGHT HOLDERS OR ANYONE DISTRIBUTING THE SOFTWARE BE LIABLE FOR
ANY DAMAGES OR OTHER LIABILITY, WHETHER IN CONTRACT, TORT OR OTHERWISE,
ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.`;

function fallbackLicense(expression) {
  if (expression === "BSD-3-Clause") return ["BSD-3-Clause", bsdThreeClauseLicense];
  if (expression === "BSL-1.0") return ["BSL-1.0", boostLicense];
  if (expression === "MPL-2.0") {
    return [
      "MPL-2.0",
      normalizeText(
        readFileSync(join(repositoryRoot, "packaging", "license-text", "MPL-2.0.txt"), "utf8"),
      ).trimEnd(),
    ];
  }
  // Every remaining missing-file expression in the locked graph offers MIT
  // as one of its alternatives (including the historical MIT/Apache spelling).
  if (/(^|\W)MIT($|\W)/.test(expression)) return ["MIT", mitLicense];
  return null;
}

function printableMetadata(value) {
  if (!value) return "not provided in package metadata";
  if (Array.isArray(value)) return value.join(", ");
  if (typeof value === "string") return value;
  if (typeof value === "object") return value.url ?? value.name ?? JSON.stringify(value);
  return String(value);
}

function addPackage(
  packages,
  ecosystem,
  name,
  version,
  expression,
  directory,
  explicitFile,
  authors,
  repository,
) {
  if (!expression || typeof expression !== "string") {
    throw new Error(`${ecosystem} package ${name}@${version} has no declared license`);
  }
  const files = licenseFiles(directory, explicitFile);
  let documents = files.map((path) => ({
    filename: relative(directory, path),
    text: normalizeText(readFileSync(path, "utf8")),
  }));
  if (documents.length === 0) {
    const fallback = fallbackLicense(expression);
    if (!fallback) {
      throw new Error(
        `${ecosystem} package ${name}@${version} has no bundled license text and no reviewed SPDX fallback`,
      );
    }
    const [selectedLicense, licenseText] = fallback;
    documents = [
      {
        filename: `reviewed-SPDX-fallback-${selectedLicense}.txt`,
        text: normalizeText(
          `Package: ${name}@${version}\n` +
            `Declared license expression: ${expression}\n` +
            `Selected license alternative: ${selectedLicense}\n` +
            `Authors/copyright attribution: ${printableMetadata(authors)}\n` +
            `Repository: ${printableMetadata(repository)}\n\n` +
            licenseText,
        ),
      },
    ];
  }
  packages.push({
    ecosystem,
    name,
    version,
    expression,
    documents,
  });
}

function collectCargo(packages) {
  const metadata = JSON.parse(
    execFileSync("cargo", ["metadata", "--locked", "--format-version", "1"], {
      cwd: repositoryRoot,
      encoding: "utf8",
      maxBuffer: 128 * 1024 * 1024,
    }),
  );
  for (const dependency of metadata.packages) {
    if (!dependency.source) continue;
    const directory = dirname(dependency.manifest_path);
    addPackage(
      packages,
      "Cargo",
      dependency.name,
      dependency.version,
      dependency.license ?? (dependency.license_file ? "SEE LICENSE FILE" : null),
      directory,
      dependency.license_file,
      dependency.authors,
      dependency.repository,
    );
  }
}

function collectNpm(packages) {
  const lock = JSON.parse(readFileSync(inputPaths.npm, "utf8"));
  for (const [packagePath, locked] of Object.entries(lock.packages ?? {})) {
    if (!packagePath.startsWith("node_modules/") || locked.dev === true) continue;
    const directory = join(repositoryRoot, "apps", "desktop", packagePath);
    const manifestPath = join(directory, "package.json");
    if (!existsSync(manifestPath)) {
      throw new Error(`npm package is not installed: ${packagePath}; run npm ci first`);
    }
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    const expression =
      typeof manifest.license === "string"
        ? manifest.license
        : typeof locked.license === "string"
          ? locked.license
          : null;
    addPackage(
      packages,
      "npm",
      manifest.name,
      manifest.version,
      expression,
      directory,
      typeof manifest.license === "object" ? manifest.license.file : null,
      manifest.author ?? manifest.contributors,
      manifest.repository,
    );
  }
}

function readUpstreamEnvironment() {
  const values = {};
  for (const line of readFileSync(inputPaths.native, "utf8").split("\n")) {
    const match = line.match(/^([A-Z0-9_]+)=(.*)$/);
    if (match) values[match[1]] = match[2];
  }
  return values;
}

function collectNative(packages) {
  const values = readUpstreamEnvironment();
  const upstreamDirectory =
    process.env.OPENFLOW_UPSTREAM_DIR ?? join(repositoryRoot, "target", "release-upstream");
  const dependencies = [
    {
      name: "whisper.cpp",
      release: values.OPENFLOW_WHISPER_CPP_RELEASE,
      revision: values.OPENFLOW_WHISPER_CPP_REVISION,
      directory:
        process.env.OPENFLOW_WHISPER_CPP_DIR ?? join(upstreamDirectory, "whisper.cpp"),
    },
    {
      name: "llama.cpp",
      release: values.OPENFLOW_LLAMA_CPP_RELEASE,
      revision: values.OPENFLOW_LLAMA_CPP_REVISION,
      directory: process.env.OPENFLOW_LLAMA_CPP_DIR ?? join(upstreamDirectory, "llama.cpp"),
    },
  ];
  for (const dependency of dependencies) {
    if (!dependency.release || !/^[0-9a-f]{40}$/.test(dependency.revision ?? "")) {
      throw new Error(`native dependency metadata is incomplete for ${dependency.name}`);
    }
    if (!existsSync(dependency.directory)) {
      throw new Error(
        `${dependency.name} source is unavailable at ${dependency.directory}; ` +
          "run scripts/fetch-inference-sources.sh first",
      );
    }
    addPackage(
      packages,
      "native",
      dependency.name,
      `${dependency.release} (${dependency.revision})`,
      "MIT",
      dependency.directory,
      null,
      `${dependency.name} contributors`,
      `https://github.com/ggml-org/${dependency.name}`,
    );
  }
}

function render(packages) {
  packages.sort((left, right) =>
    [left.ecosystem, left.name, left.version].join("\0").localeCompare(
      [right.ecosystem, right.name, right.version].join("\0"),
    ),
  );
  const documents = new Map();
  for (const dependency of packages) {
    for (const document of dependency.documents) {
      const digest = sha256(document.text);
      const current = documents.get(digest) ?? { text: document.text, users: [] };
      current.users.push(
        `${dependency.ecosystem}:${dependency.name}@${dependency.version}:${document.filename}`,
      );
      documents.set(digest, current);
    }
  }

  const lines = [
    expectedHeader(inputDigests()),
    "",
    "The package index records every locked Cargo dependency, every production npm",
    "dependency, and the two statically linked native inference runtimes. Exact duplicate",
    "license documents are stored once and referenced by SHA-256.",
    "",
    "PACKAGE INDEX",
    "=============",
  ];
  for (const dependency of packages) {
    const references = dependency.documents
      .map((document) => `${document.filename}=sha256:${sha256(document.text)}`)
      .join(", ");
    lines.push(
      `${dependency.ecosystem} | ${dependency.name} | ${dependency.version} | ` +
        `${dependency.expression} | ${references}`,
    );
  }
  lines.push("", "LICENSE DOCUMENTS", "=================");
  for (const [digest, document] of [...documents.entries()].sort(([left], [right]) =>
    left.localeCompare(right),
  )) {
    lines.push(
      "",
      `--- sha256:${digest} ---`,
      `Used by: ${document.users.sort().join(", ")}`,
      "",
      document.text.trimEnd(),
    );
  }
  return `${lines.join("\n")}\n`;
}

const packages = [];
collectCargo(packages);
collectNpm(packages);
collectNative(packages);
const generated = render(packages);

if (mode === "--write") {
  writeFileSync(outputPath, generated);
  process.stdout.write(
    `Wrote ${relative(repositoryRoot, outputPath)} for ${packages.length} dependencies\n`,
  );
} else {
  checkInputs();
  if (readFileSync(outputPath, "utf8") !== generated) {
    throw new Error(
      "third-party license aggregate is stale; run " +
        "scripts/generate-third-party-licenses.mjs --write and review the result",
    );
  }
  process.stdout.write(`Verified third-party licenses for ${packages.length} dependencies\n`);
}
