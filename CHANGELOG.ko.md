# Changelog

<p>
  <a href="CHANGELOG.md"><img src="https://img.shields.io/badge/lang-English-blue" alt="English"></a>
</p>

## [0.2.0](https://github.com/mobis-oss/ssam/compare/v0.1.0...v0.2.0) - 2026-11-04

### Breaking changes

- SSAM 패키지 형식이 0.6.0으로 업데이트되었습니다.
  `ssam-wrap` 0.1.0으로 만든 패키지(형식 0.2.0)는 다시 만들어야 합니다.
- Remocon API 버전이 2로 업데이트되었습니다.
  - RPC 이름과 응답 스키마를 정리했고, 응답은 typed protobuf 메시지를 사용합니다.
  - 요청은 `x-ssam-api-version` metadata에 API 버전을 담습니다.
  - `ssamd`는 abstract unix socket으로 API를 제공합니다.
  - `ssamd`는 설정된 그룹의 RPC peer만 받습니다.
- `ssam install`은 force 플래그 대신 `--downgrade`를 받습니다.
- `ssam-wrap -o/--output`은 선택 옵션이고, 지정한 경로를 그대로 사용합니다.
- 패키지 설정 구조를 바꿨습니다.
  bridge 전용 필드는 `[container.network.bridge]` 아래로 모였고, 알 수 없는 필드는 거부합니다.
- 오류 처리 정책이 전반적으로 fail-ignore에서 fail-stop으로 바뀌었습니다.
  보안 정책(*seccomp*, MAC) 적용, 데이터 디렉터리 준비, 설정 파싱 중 오류가 나면 해당 작업을 중단합니다.
- 최소 지원 Rust 버전은 1.99입니다.

### Added

- `[container.network]`로 컨테이너 네트워크를 설정합니다: port publish를 포함한 bridge 모드, 컨테이너별 internal bridge, 외부 network namespace 모드.
- 패키지 데이터를 지우는 `ssam wipe-data`.
- `ssamd`, `ssam`, `ssam-wrap`의 `--version`.
- `ListPackages`, `ssam list`, `ssam pkg-info`의 패키지 위치(builtin, downloaded, local file).
- `ssam-wrap` 패키지 파일시스템 소스로 쓸 수 있는 이미지 형식 추가, 소스 이미지 아키텍처를 고르는 `--src-oci-arch`.
- runtime integrity policy와 RPC peer 그룹을 위한 `ssamd` 설정.
- architecture, guide, reference 문서.

### Changed

- `ssamd`는 재시작 후 실행 중이던 패키지를 복구합니다.
- `ssamd`는 패키지 생명주기 요청을 병렬로 처리합니다.
- 번들 생성에 실패한 패키지는 Broken 상태가 됩니다.
- Bazel 빌드는 Bazel 9.3.1로 고정되고 crate 버전을 `Cargo.toml`에서 가져옵니다.
- 사용하지 않는 zip 압축 관련 의존성을 제거했습니다.

### Security

- 서명은 *dm-verity* 파라미터 테이블(루트 해시 포함) 전체를 대상으로 합니다.
- 서명 검증은 패키지 파일을 한 번만 읽습니다.
- 패키지 파싱은 인증 전에 메모리 할당 크기를 제한합니다.
- `ssamd`는 패키지 이름, 데이터 디렉터리, 설치 경로에서 path traversal과 일반 파일이 아닌 경로를 검증합니다.
- `ssamd`는 overlay mount 옵션과 *dm-verity* device 이름을 injection으로부터 보호합니다.
- `ssamd`는 *OCI Runtime bundle* 디렉터리를 소유자만 접근하도록 제한합니다.
- 기본 *seccomp* 정책은 user namespace 생성을 막습니다.
- 예시로 제공하는 aarch64 musl 빌드 설정은 바이너리를 static-pie로 링크해 코드에도 ASLR이 적용됩니다.

### Fixed

- 공백이 들어간 경로의 `veritysetup` 인자.
- 손상된 *EROFS*·*ext4* superblock의 범위를 벗어난 block size로 인한 overflow panic.
- 4 KiB보다 큰 page size의 호스트에서 만든 패키지를 4 KiB page 커널에서 불러오지 못하던 문제.
  `ssam-wrap`은 4096바이트 block size로 *EROFS* 이미지를 만듭니다.
- *project quota* 커널 ABI 타입 불일치.
- setup·install 실패 후 device-mapper device, 패키지 파일, 볼륨 정리.
- `remove_data` 업그레이드 실패 시 데이터 보존.
- 번들 준비와 `wipe-data` 요청의 동시 처리.
- D-Bus `ActiveState` stream이 닫힌 경우의 처리.

## [0.1.0](https://github.com/mobis-oss/ssam/releases/tag/v0.1.0) - 2026-05-15

최초 공개 릴리즈.
