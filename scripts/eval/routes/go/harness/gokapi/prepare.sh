#!/bin/sh
# routes-v1.2's round-3 held-out gokapi: its webserver embeds two WebAssembly builds (internal/webserver/web/main.wasm and
# e2e.wasm) that aren't committed, so empty stand-ins in the container's copy let the Go code build; and with no
# configuration file it serves its setup wizard instead of its webserver, so a configuration is written here: SQLite in
# its data directory, username authentication, no TLS, as the setup wizard writes one. The routes the webserver
# registers depend on neither (an OAuth login adds two routes; this configuration has none). The program runs in its
# package directory (list.sh), where its relative `config` and `data` directories are.
: > internal/webserver/web/main.wasm
: > internal/webserver/web/e2e.wasm
mkdir -p cmd/gokapi/config cmd/gokapi/data
cat > cmd/gokapi/config/config.json <<'JSON'
{
  "Authentication": { "Method": 0, "SaltAdmin": "kcroutesadminsaltkcroutesadminsa", "SaltFiles": "kcroutesfilessalt", "Username": "admin",
    "HeaderKey": "", "OauthProvider": "", "OAuthClientId": "", "OAuthClientSecret": "", "OauthGroupScope": "",
    "OAuthRecheckInterval": 12, "HeaderUsers": null, "OAuthGroups": [], "OauthUsers": [] },
  "Port": "127.0.0.1:53842",
  "ServerUrl": "http://127.0.0.1:53842/",
  "RedirectUrl": "",
  "PublicName": "Gokapi",
  "DataDir": "data",
  "DatabaseUrl": "sqlite://data/gokapi.sqlite",
  "ConfigVersion": 22,
  "MaxFileSizeMB": 102400,
  "MaxMemory": 50,
  "ChunkSize": 45,
  "Encryption": { "Level": 0, "Cipher": null, "Salt": "", "Checksum": "", "ChecksumSalt": "" },
  "MaxParallelUploads": 3,
  "UseSsl": false,
  "PicturesAlwaysLocal": false,
  "SaveIp": false,
  "IncludeFilename": false,
  "MinLengthPassword": 8
}
JSON
