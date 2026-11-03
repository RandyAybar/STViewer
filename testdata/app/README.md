# testdata/app

`demo.mbox` is the demo mailbox used for manual testing of the desktop app and for the
screenshots in `docs/screenshots/`. It is generated deterministically (seeded PRNG) by

```sh
node app/scripts/make-fixtures.mjs              # rewrites testdata/app/demo.mbox
node app/scripts/make-fixtures.mjs --large N    # testdata/large/bulk-N.mbox (git-ignored)
```

All people and organisations are fictional (`*.example` domains); the mailbox owner is
"Jordan Example <jordan@stviewer.example>". The embedded images are the app icon.
Covered by the project license.
