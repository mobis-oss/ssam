# ssamd CLI Reference

<p>
  <a href="ssamd.ko.md"><img src="https://img.shields.io/badge/lang-한국어-blue" alt="Korean"></a>
</p>

Run `ssamd` without arguments to start the daemon.

## Options

| Option | Description |
| --- | --- |
| `-V`, `--version` | Print build and compatibility versions. |

## Version output

```console
$ ssamd --version
Daemon version:           0.1.0
RPC API version:          1
Package format:           0.6.0
OCI Runtime Spec Version: 1.1.0
```

| Field | Meaning |
| --- | --- |
| `Daemon version` | Version declared by the `ssamd` workspace package. |
| `RPC API version` | Current version of the SSAM Remocon API. |
| `Package format` | Version of the SSAM package file format. |
| `OCI Runtime Spec Version` | *OCI Runtime spec* version supported by `ssamd`. |
