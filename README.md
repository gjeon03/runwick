# Runwick

Runwick(런윅)은 여러 AI 코딩 CLI(Claude Code·Codex)와 셸을 프로젝트 단위로 오가며, 작업과 맥락을 잃지 않게 이어 주는 macOS용 네이티브 앱이다. Rust와 egui로 만들었고 Electron을 쓰지 않는다.

## 할 수 있는 것

- **프로젝트 → 작업 공간 → 세션**: 폴더를 열면 기본 공간에서 바로 시작한다. Claude Code·Codex·셸을 원래 CLI 화면 그대로 쓴다(한글 입력, 색, 붙여넣기, 마우스).
- **창을 닫아도 실행 유지**: 실행은 별도 호스트 프로세스(`runwick host`)가 가진다. 창을 다시 열면 같은 실행에 붙고, 메뉴 막대에서 확인할 일과 실행 수를 본다. `작업까지 종료`는 실제로 멈췄는지 확인한 뒤 끝낸다. 재부팅 뒤에는 기록과 마지막 화면을 복원하고, 재개할 세션은 직접 고른다.
- **기록·검색·핀**: CLI가 남기는 대화 파일을 읽어 한국어 부분 검색이 되는 기록을 만든다. 메시지와 터미널 발췌를 핀으로 고정한다.
- **파일과 문서**: 파일 트리, 이름 검색, Markdown·텍스트 편집과 미리보기, 코드 보기. CLI가 같은 파일을 바꾸면 덮어쓰지 않고 3열 비교를 보여 준다.
- **위키·타임라인**: 프로젝트 위키(링크 그래프, AI 변경 제안 검토)와 날짜별 타임라인, 이어하기 카드. AI 정리는 프로젝트별로 켜야 돌고 배터리에서는 보류한다.
- **협업**: CLI 안에서 `runwick delegate`로 다른 CLI에 일을 맡기고 `wait`/`result`로 결과를 받는다. 작업자를 잠깐 보거나 직접 대화로 입력권을 가져올 수 있다. 분리해서 작업(git worktree)과 통합 충돌 처리.
- **공통 환경변수와 세션 새로고침**: 환경변수는 하나의 공통 설정이고 비밀값은 macOS 키체인에 둔다. 바뀐 설정은 같은 대화를 유지한 채 새로고침으로 적용한다.
- **GitHub 백업·동기화**: 비공개 저장소에 기록·핀·위키·타임라인·역할·설정(비밀값 제외)을 올리고, 다른 컴퓨터에서 받아 이어 쓴다. 두 컴퓨터에서 같은 것을 고치면 진 쪽 내용을 `겹친 변경`에 남기고, 다른 컴퓨터에서 온 환경변수 값은 검토한 뒤에만 쓴다. 이 컴퓨터 디스크에 서명된 로컬 백업도 만든다.
- **여러 기기**: SSH로 다른 Mac의 Runwick에 연결해 같은 사이드바에서 그 기기의 작업 공간을 쓴다(`runwick host --stdio`).

## 실행하기

필요한 것: macOS 11 이상, [Rust](https://rustup.rs)(stable), Xcode 또는 Command Line Tools.

```sh
cargo run -p runwick            # 앱 실행 (호스트는 자동으로 시작)
sh scripts/bundle-macos.sh      # dist/Runwick.app 만들기 (release 빌드, ad-hoc 서명)
open dist/Runwick.app
```

- Xcode 사용권에 아직 동의하지 않았다면 링크 단계가 실패한다. `sudo xcodebuild -license accept`로 동의하거나, 명령마다 `DEVELOPER_DIR=/Library/Developer/CommandLineTools`를 붙여 Command Line Tools로 빌드한다.
- 데이터는 `~/.runwick`(권한 0700)에 있다. `RUNWICK_HOME`으로 다른 폴더를 쓸 수 있어 시험용 실행을 분리할 수 있다.
- 테스트: `cargo test --workspace`

### CLI 안에서 쓰는 명령

Runwick이 띄운 터미널에는 `runwick` 명령이 PATH에 있다.

| 명령 | 내용 |
| --- | --- |
| `runwick delegate --tool codex "할 일"` | 다른 CLI에 일을 맡기고 작업자 ID를 출력 |
| `runwick workers` · `status` | 맡긴 작업자와 협업 상태 |
| `runwick wait <id>` · `result <id>` | 끝날 때까지 기다리기, 결과(요약·바뀐 파일·검증·남은 일) |
| `runwick send <id> "후속 지시"` | 자기 작업자에게만 보낼 수 있다 |
| `runwick report --summary "…"` | 작업자가 결과를 남긴다 |

## 구조

| 경로 | 역할 |
| --- | --- |
| `crates/runwick-core` | 통신 규격(프레임·요청·이벤트), 기능별 API 타입, 데이터 경로, 터미널 출력 안전장치 |
| `crates/runwick-host` | 실행을 유지하는 호스트: PTY와 터미널 상태, SQLite 저장소, 기록 수집, 위키·타임라인·자동 작업, 협업, 동기화·백업, 기기 |
| `crates/runwick-app` | egui 화면: 셸(제목 막대·사이드바·탭·패널), 터미널, 기능별 화면 |
| `crates/runwick` | 실행 파일: 앱, `host`, `menubar`, 에이전트용 하위 명령 |
| `assets/` | 글꼴(Geist, JetBrains Mono, Lucide — 각 라이선스 동봉)과 앱 아이콘 |

보안 범위:

- 호스트 소켓과 데이터 폴더는 사용자 본인만 쓸 수 있다.
- 환경변수·동기화·백업·세션 입력 같은 일은 Runwick 창만 할 수 있다. 호스트는 창 토큰과 함께 연결한 프로세스가 Runwick 실행 파일인지, Runwick이 관리하는 실행 안에서 온 것이 아닌지, 원격이면 SSH 로그인을 거쳤는지 확인한다.
- CLI 안의 에이전트는 자기 세션의 협업 명령(맡기기·결과·자기 작업자에게 보내기)만 쓸 수 있고, 다른 프로젝트나 다른 세션에는 손댈 수 없다.
- 다른 컴퓨터에서 동기화로 온 환경변수는 검토하기 전까지 적용하지 않는다. 호스트가 프로젝트 폴더에서 git을 실행할 때는 저장소 설정의 hook·fsmonitor·filter를 끈다.
- 같은 사용자로 실행되는 프로그램은 키체인과 데이터 파일을 직접 읽을 수 있으므로, 이 제한은 실수와 프롬프트 주입을 막는 장치이지 완전한 격리가 아니다.
- Runwick 터미널 안에서 띄운 Runwick 창은 창 권한을 받지 않는다. 개발 중에는 `open`이나 Finder로 실행한다.
- 평소에는 `scripts/bundle-macos.sh`로 만든 `Runwick.app`을 쓴다. 번들은 hardened runtime으로 서명되고 검증용 훅이 빠진 release 빌드다. `cargo run`으로 띄운 개발 빌드에는 이 보호가 없으므로 시험용 `RUNWICK_HOME`과 함께 쓴다.

## 문서

기획 문서는 구현 전 설계 기록이다. 구현에서 정한 것은 [결정 기록](docs/decisions.md)에 남겼다.

| 문서 | 내용 |
| --- | --- |
| [UI 디자인 시안](docs/design/README.md) | 51개 흐름 화면과 공통 부품, 시각 기준(Graphite Wick). 브라우저로 바로 열어 볼 수 있음 |
| [전체 제품 기획](docs/product-spec.md) | 제품 범위, 17개 요구사항, 기록·지식·설정·책임 경계 |
| [협업·오케스트레이션](docs/orchestration.md) | 팀 선언 없이 시작하는 위임, 입력권, 작업 공간, 결과·실패·복구 |
| [화면 명세](docs/ui-spec.md) | 15개 화면/패널의 정보·행동·상태·반응형 |
| [사용 흐름·수용 기준](docs/user-flows.md) | 15개 흐름과 예외 처리, 요구사항→화면→검증 추적표 |
| [결정 기록](docs/decisions.md) | 사용자 결정, 구현에서 정한 기술 선택, 남은 선택지 |
| [제품 방향](docs/product-direction.md) | 사용 흐름, 차별점 가설 |
| [데이터 이전](docs/data-migration.md) | 기존 기록 이전에 대한 초기 검토 (앱 기능에서는 제외됨) |
| 조사: [Herdr](docs/research/herdr.md) · [협업 코드 비교](docs/research/collaboration-models.md) · [원격](docs/research/orca-remote.md) · [세션 새로고침](docs/research/session-refresh.md) · [기존 도구 검토](docs/research/pinloom.md) | 설계 근거 |

## 남은 일과 알려진 한계

- 다른 Mac에 배포하려면 Developer ID 서명과 공증이 필요하다(지금 번들은 ad-hoc 서명, hardened runtime).
- 실제 두 대의 Mac 사이 SSH 연결 성공 경로는 시뮬레이션 기기와 단위 테스트로만 확인했다(이 Mac은 원격 로그인이 꺼져 있다).
- 실행을 멈출 때 세션에서 완전히 떨어져 나간 프로그램은 죽이지 않고 "계속 실행 중"으로 알린다(Codex 공유 서버 같은 프로그램 보호). 다만 Apple 기본 프로그램(예: `/usr/bin/perl`)으로 떨어져 나간 경우는 macOS가 정보를 숨겨 알리지 못한다.
- 호스트가 프로젝트 폴더에서 git을 실행할 때 저장소 설정의 실행 항목을 끄지만, 같은 순간에 설정 파일을 바꿔 치는 경쟁은 완전히 막지 못한다(그 폴더에 쓸 수 있는 프로그램이 계속 경쟁해야 가능).
- Codex 입력창에 번호 목록을 쓰는 동안에는 보내기 전까지 "선택 필요"로 보일 수 있다.
- 밝은 테마, 모바일 앱, 의미 검색(임베딩)은 만들지 않았다.
