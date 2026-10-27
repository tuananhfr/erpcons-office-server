# Blank templates

The file a blank document (`POST /api/v1/documents` with `{kind}`) starts as. Each was written by the
fork that edits it, then opened again by that fork's CLI (`info`), on 27/10:

| File | Fork (branch `tuananhfr`) | Command | sha256 |
|---|---|---|---|
| `blank.docx` | wordcraft `049cf96f86a613107b7c13cb6642aaf50c1cab03` | `wordcraft-cli run --save blank.docx` | `71b94b5afeaea302aadb1e83bff9d028971b39a0ee7611e5740b94ffaa36d9df` |
| `blank.xlsx` | gridcraft `0d746f3026a274d1b0ed4538a56d39ecf30783f4` | `gridcraft-cli run --out blank.xlsx --quiet` | `6690e1424fca3695b95b483195b455febc0cbebe6c527bf88607836f2eb5a44d` |
| `blank.pptx` | deckcraft `936abcb4dbb3c98b5923c71f1a792f96c99077d7` | `deckcraft-cli run --save blank.pptx` | `4f42e8eaee3b39d55688b33eb65bbf71b8bb289791b84b7c7af872ba8ed07cb2` |
| `blank.pdf` | hand-written (one empty A4 page, PDF 1.4); opened by pdfcraft `ef27ca3f4fe2c107bdaa90802bc96b8494e04248` `pdfcraft-cli info blank.pdf` | | `6b6ac508ea1327b4812e181a3976d3032d15ad05692c1622468bec575e00b207` |

The CLIs were built in the ERPCons build image from clean checkouts (`cargo build --release --locked -p <fork>-cli`).
The script is `make-templates.sh` next to them; from `erpcons-workspace`:

```sh
bash erpcons-office/build/run.sh bash /ws/erpcons-office-server/crates/documents/templates/make-templates.sh
```

To refresh a template, rerun it from the fork's new release commit and update this table. The files are binary
(`.gitattributes`); the server's own checks (signature, ZIP central directory) must still accept them, which the
document API tests cover.
