#!/usr/bin/env bash
# Runs INSIDE the build image: builds the 4 fork CLIs from the tuananhfr checkouts and writes the
# blank templates of erpcons-office-server, then has each fork open its template again.
#   bash erpcons-office/build/run.sh bash /ws/erpcons-office-server/crates/documents/templates/make-templates.sh
set -euo pipefail
ws=/ws
out=$ws/erpcons-office-server/crates/documents/templates

export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-6}
for fork in wordcraft gridcraft deckcraft pdfcraft; do
  echo "== $fork $(git -C "$ws/$fork" rev-parse HEAD) dirty=$(git -C "$ws/$fork" status --porcelain | wc -l)"
  (cd "$ws/$fork" && CARGO_TARGET_DIR=/target/wt/office-templates-$fork cargo build --release --locked -q -p "$fork-cli")
done
W=/target/wt/office-templates-wordcraft/release/wordcraft-cli
G=/target/wt/office-templates-gridcraft/release/gridcraft-cli
D=/target/wt/office-templates-deckcraft/release/deckcraft-cli
P=/target/wt/office-templates-pdfcraft/release/pdfcraft-cli
cd "$out"
rm -f blank.docx blank.xlsx blank.pptx blank.pdf
"$W" run --save blank.docx
"$G" run --out blank.xlsx --quiet
"$D" run --save blank.pptx
python3 - <<'PY'
objs = [
    b"<< /Type /Catalog /Pages 2 0 R >>",
    b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << >> >>",
]
pdf = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
offsets = []
for i, body in enumerate(objs, 1):
    offsets.append(len(pdf))
    pdf += b"%d 0 obj\n" % i + body + b"\nendobj\n"
xref = len(pdf)
pdf += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
for off in offsets:
    pdf += b"%010d 00000 n \n" % off
pdf += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref)
open("blank.pdf", "wb").write(bytes(pdf))
PY
echo "== reopen"
"$W" info blank.docx
"$W" convert blank.docx /tmp/blank-roundtrip.docx
"$G" info blank.xlsx
"$D" info blank.pptx | head -30
"$P" info blank.pdf
sha256sum blank.docx blank.xlsx blank.pptx blank.pdf
ls -l
