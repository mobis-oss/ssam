# ssamd CLI 레퍼런스

<p>
  <a href="ssamd.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

인자 없이 `ssamd`를 실행하면 데몬을 시작합니다.

## 옵션

| 옵션 | 설명 |
| --- | --- |
| `-V`, `--version` | 빌드 및 호환성 버전을 출력합니다. |

## 버전 출력

```console
$ ssamd --version
Daemon version:           <DAEMON_VERSION>
RPC API version:          <RPC_API_VERSION>
Package format:           <PACKAGE_FORMAT_VERSION>
OCI Runtime Spec Version: <OCI_RUNTIME_SPEC_VERSION>
```

| 항목 | 의미 |
| --- | --- |
| `Daemon version` | 워크스페이스에서 관리하는 `ssamd` 패키지에 선언된 버전입니다. |
| `RPC API version` | SSAM Remocon API의 현재 버전입니다. |
| `Package format` | SSAM 패키지 파일 형식 버전입니다. |
| `OCI Runtime Spec Version` | `ssamd`가 지원하는 *OCI Runtime spec* 버전입니다. |
