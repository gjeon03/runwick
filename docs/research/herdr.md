# Herdr 조사와 Runwick 적용 후보

조사일: 2026-11-05

Herdr에서 배울 핵심은 Rust라는 언어만이 아니라 **기존 CLI를 유지하면서 프로젝트 전환·실행 지속·사용자 개입을 쉽게 만드는 구조**다. Runwick에서는 이 흐름에 영구 기록과 작업 재개, 결과 검토를 연결하는 방향을 검토한다.

팀 선언 유무와 실제 위임·입력·상태·개입 경로의 추가 소스 조사는 [협업 구조 비교](collaboration-models.md)에 정리했다. Core의 실행 관계와 App의 작업자 표시를 분리해 분석하며, 이를 바탕으로 Runwick의 [협업 상세 설계](../orchestration.md)를 제안했다.

## 조사 범위와 버전

| 대상 | 기준 |
| --- | --- |
| Herdr App 소스 | [`e77f112ac2ea136b84fe510efa42d41163bfd776`](https://github.com/herdrdev/herdr-app/tree/e77f112ac2ea136b84fe510efa42d41163bfd776), 소스 Cargo 버전 0.1.12 |
| Herdr Core 소스 | [`c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a`](https://github.com/herdrdev/herdr-core/tree/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a) |
| 기존 Herdr TUI 소스 | [`ee5bfb5041bb3e4a49d591c72308aae3c81ecdde`](https://github.com/herdrdev/herdr/tree/ee5bfb5041bb3e4a49d591c72308aae3c81ecdde) |
| 실행 확인 | macOS 공식 릴리스 App 0.1.13, Core 0.1.18, 배포 manifest의 SHA256 대조 |
| 공식 설명 | [홈](https://herdr.dev/), [App 가이드](https://herdr.dev/guide/), [Core](https://herdr.dev/core/), [TUI](https://herdr.dev/tui/) |

App·Core·기존 TUI는 구분해서 봐야 한다. App은 Core를 사용하지만 기존 TUI와 서버·상태 저장을 자동으로 공유하는 하나의 프로그램은 아니다. 소스 분석 버전과 실행한 App 릴리스도 동일하다고 가정하지 않는다.

## 배우거나 발전시킬 항목

| 항목 | 확인한 사실 | Runwick 적용 후보 |
| --- | --- | --- |
| 프로젝트 중심 작업 공간 | Space 안에 Tab·Pane을 구성하고 현재 폴더에 따라 이름을 연결 [H1] | 프로젝트·작업·실행의 관계를 쉽게 탐색 |
| worktree 시작 | 메뉴에서 branch와 checkout을 만들고 새 Space에서 실행 [H2] | 폴더 생성부터 CLI 시작까지 연결. 의존성·환경·포트 준비는 별도 범위로 판단 |
| 실행 지속성 | UI와 Core, PTY를 소유하는 holder가 분리 [H3] | 창 종료와 작업 종료를 구분하고 재연결을 신뢰할 수 있게 설계 |
| 상태 보고와 알림 | Core가 OSC 7501 보고를 받으며 App은 blocked·done·working 등의 상태를 표시 [H4] | 입력 요청을 놓치지 않되 실행 상태와 결과 검토를 혼동하지 않기 |
| 하위 에이전트 | 부모·자식 terminal 관계, sidebar 표시, peek·keep 명령 [H1][H4] | 같은 작업의 여러 실행을 묶고 요청만 추려보기 |
| 자원 사용 | 숨은 터미널은 출력 파싱과 화면 재그리기를 구분, 출력 갱신 배치와 scrollback 상한 [H5] | 세션 수 증가 시 유휴·출력 부하를 제한하고 실제 측정 |
| 개인화 | JavaScript mod가 네이티브 요소를 구성, CLI로 앱 상태 조회·제어 [H1] | 안정된 설정·확장 계약, 변경 미리보기와 복구 |
| 변경 검토 | 기본 Changes mod가 파일 diff·commit 제공 [H6] | 변경 범위·검증 결과·수용 기준을 작업 단위로 연결 |
| 원격 연결·API | Core의 원격 연결과 schema 기반 API, 호환성 테스트 [H7] | 초기 범위를 넓히기보다 코어 경계 설계의 참고로 활용 |

## 실제 작업 흐름에서 확인한 것

격리한 설정·데이터 디렉터리와 임시 Git 저장소를 사용했다. 실제 Claude/Codex 작업 대신 OSC 7501 상태를 내보내는 모의 프로그램으로 실행 수명과 상태 표시를 확인했다.

1. 첫 실행 안내에서 실행 지속성, 선택적 원격 연결, Files·Changes, command palette와 에이전트 설정을 설명한다.
2. 임시 저장소로 이동하자 프로젝트 Space 이름이 폴더를 따라갔다.
3. 프로젝트 메뉴에서 새 worktree를 만들고, sidebar에서 원래 저장소와 연결된 공간으로 확인했다.
4. `working`·`blocked` 상태를 보고하는 실행을 나란히 만들고, 부모 아래에 하위 실행이 표시되는 것을 확인했다.
5. 앱을 종료하고 다시 열었을 때 기존 PTY와 foreground 프로세스 PID가 유지됐다.
6. Core의 `server restart` 뒤에도 기존 프로그램과 holder가 유지되고 App이 다시 연결됐다.

ANSI 색상, 한글 출력과 붙여넣기는 확인했다. **실제 한글 IME 조합, OS 재부팅, SSH, 배터리 사용량, 실제 AI의 장시간 작업은 확인하지 않았다.** `terminal.peek` 명령은 성공했지만 후반 UI 자동화 연결 문제로 화면 검증은 완료하지 못했다. 전체 서버 중지 후 재시작도 완전한 복원 검증으로 계산하지 않았다.

App의 worktree·spaces·agents·palette·picker JavaScript 로직 테스트 29개는 통과했다. 모의 환경의 로직 테스트이며 네이티브 앱 전체 통합 테스트나 Rust 전체 테스트 통과를 의미하지 않는다. 조사용 프로세스는 종료했고 실제 프로젝트 데이터는 변경하지 않았다.

## Rust 기반이라는 설명의 정확한 범위

App은 Rust와 GPUI를 사용하지만 모든 동작이 Rust 소스만으로 이루어진 것은 아니다. 기본 패널과 레이아웃에 JavaScript mod를 사용하며 QuickJS에서 네이티브 UI 요소를 구성한다. 터미널에는 Ghostty/Zig 기반 코드가 관여한다. 이 mod 방식은 브라우저 DOM이나 Node 기반 UI와는 다르다. [H1][H5]

따라서 Runwick의 라이브러리 선택은 “Herdr처럼 Rust”라는 설명만으로 결정할 수 없다. 네이티브 입력·렌더링 품질, 확장 방식, 의존 라이브러리와 패치 유지 부담을 함께 비교해야 한다.

## 실행 복원과 영구 기록의 차이

holder는 PTY와 프로세스를 유지하고 Core 재시작에 대응한다. 그러나 holder나 OS까지 종료된 상황은 다르다. `keep` 복원은 새 셸을 여는 경로이며 모든 기존 명령을 자동으로 재실행한다는 의미가 아니다. [H3]

Core에는 종료 terminal 기록의 24시간 보존 기준과 최대 500개 제한이 있으며, 살아 있는 부모가 아직 확인하지 않은 자식 등에 대한 예외가 있다. 제한된 화면·출력 보존은 provider의 구조화된 대화 원문과 프로젝트 지식을 영구 보관하는 기능과 다르다. [H4]

Runwick에서는 살아 있는 실행에 재연결하기, provider 세션을 새 프로세스에서 재개하기, 과거 기록을 읽기를 각각 표시하는 것이 좋다. 위키와 타임라인은 실행 수명과 독립된 저장 대상으로 검토한다.

## 성능 주장 해석

App 소스에는 출력 갱신을 약 33ms 단위로 모으고 최근 입력에는 빠르게 반응하는 경로, 변경 행 캐시, 10,000줄·10,000,000바이트 scrollback 상한이 있다. 이는 최적화 방법의 근거이지 Runwick이나 Pinloom 대비 성능 수치는 아니다. [H5]

공식 [CPU 개선 글](https://herdr.dev/blog/ten-agents-three-clients-95-percent-less-cpu/)의 89–95% 감소는 기존 TUI 버전 간 특정 시험 조건의 결과다. Electron 또는 Pinloom과의 비교로 인용하지 않는다. App 0.1.13의 [메모리 사용량 보고](https://github.com/herdrdev/herdr-app/issues/560)도 서로 다른 기기의 사용자 보고이며 통제된 비교 실험은 아니다.

## 사용자 보고에서 도출한 검증 항목

아래 링크는 실제 사용자 요구나 문제 보고다. 모든 현상을 직접 재현한 것은 아니며, 현행 버전에 항상 존재하는 결함으로 단정하지 않는다.

| 보고 | 의미 | Runwick에서 검증할 것 |
| --- | --- | --- |
| [App 460](https://github.com/herdrdev/herdr-app/issues/460) | 하위 실행과 부모 집계 상태가 어긋날 수 있다는 문제 | 부모가 응답을 끝내도 자식 실행·미검토 결과가 사라지지 않는지 |
| [App 461](https://github.com/herdrdev/herdr-app/issues/461) | AI가 현재 사용 중인 UI를 수정하는 불편 | 개인화 변경을 별도 환경에서 확인한 뒤 적용·복구할 수 있는지 |
| [Core 15](https://github.com/herdrdev/herdr-core/issues/15) | provider resume 정보와 재개 지원 요구 | 재개 ID·실행 옵션·작업 경로·사용자의 종료 의도를 보존하는지 |
| [Core 122](https://github.com/herdrdev/herdr-core/issues/122) | 현재 turn·상태에 입력을 묶는 보호 요구 | 오래된 승인 화면에서 엉뚱한 프롬프트에 입력하지 않는지 |
| [App 562](https://github.com/herdrdev/herdr-app/issues/562) | 한글 IME 문제 보고 | 조합 중 입력·삭제·이동·한영 전환·복사를 실제로 시험하는지 |

## 그대로 채택하지 않을 수 있는 부분

기본 Changes mod의 commit은 `git add -A`를 사용하는 경로다. 조사한 구현만으로는 선택한 변경·수용 기준·테스트 증거까지 연결하는 결과 검토 흐름이 충분하다고 보기 어렵다. Runwick의 검토 경험을 설계할 때 별도로 비교할 지점이다. [H6]

개인 mod는 호스트 기능을 실행할 수 있는 신뢰된 코드다. 복제된 mod는 기본 mod 업데이트가 자동으로 반영되지 않는다. 유연성은 장점이지만, 사용자가 이미 겪은 “개인 기능 유지 때문에 업데이트가 어렵다”는 문제를 다시 만들 수 있으므로 설정·확장의 경계를 좁히는 안을 검토한다. [H1]

worktree는 Git 작업 디렉터리의 분리다. 의존성·환경변수·개발 서버 포트의 자동 격리나 보안 sandbox를 보장하지 않는다. 자식 실행 관리 또한 목표 분해·작업 품질·자동 merge까지 해결하는 오케스트레이션과 같지 않다.

## 다음 비교의 우선순위

1. 프로젝트에서 별도 작업 공간과 CLI를 만드는 흐름이 얼마나 짧고 명확한가.
2. 여러 실행 중 사용자 개입이 필요한 것을 놓치지 않는가.
3. 실행 수명과 기록 보존·재개 상태를 정확히 설명할 수 있는가.
4. 실제 한글 입력과 색상·복사·붙여넣기가 기존 CLI 경험을 유지하는가.
5. 개인화와 업데이트가 충돌하지 않으며 백그라운드 비용을 확인할 수 있는가.

## 2026-11-07 Codex 실행과 협업 연결 재조사

로컬 소스와 설치된 `codex-cli 0.167.0`만 조사했다. 실제 모델 세션은 실행하지 않았고, 설정·프롬프트 검증은 임시 `CODEX_HOME`에서 수행했다.

| 대상·revision | 확인한 내용과 소스 위치 |
| --- | --- |
| App `adf94cb2c9b6f1cadbb31c51f84114b1aa332f0a` | `defaults/programs.json:6`은 `codex --no-daemon`. `defaults/AGENTS.md:774`는 공유 daemon 때문에 후속 세션이 첫 터미널의 신원·수명에 묶일 수 있다고 설명한다 |
| Core `9ce385d9c8abb041dd4437476d770365b3fb5282` | `crates/herdr-core/src/SKILL.md:270`도 Codex에 `--no-daemon`을 요구한다. `api/run.rs:218`은 `HC_TERMINAL_ID`·`HC_TERMINAL_SOCKET`을 넣고, `client.rs:265`는 연결할 Core의 소켓과 환경의 주소를 비교한다. `paths.rs:28`의 소켓 선택은 `HC_SOCKET` → 설정 → 기본 경로 순서다(세 파일 모두 `crates/herdr-core/src/` 아래) |
| Core 같은 revision | `crates/herdr-core/src/SKILL.md:16`은 `hc run/send/wait`로 위임하고 환경의 터미널 주소로 보고하도록 안내한다. CLI 설치 경로는 같은 파일 `:235`의 `~/.local/bin`. `docs/security.md:3,20`은 Core 자체가 sandbox가 아니며, 소켓에 연결한 같은 사용자 프로그램에 별도 클라이언트 권한 검사를 하지 않는다고 명시한다. Runwick의 호출자 보안 모델과 동일하지 않다 |
| TUI `ee5bfb5041bb3e4a49d591c72308aae3c81ecdde` | `src/detect/mod.rs:153`은 실행 파일 `codex`, `src/agent_resume.rs:212`는 `codex resume <id>`를 만든다. `src/pane.rs:202`와 `src/integration/env.rs:29`는 pane ID·소켓·실행 파일 경로를 환경으로 전달한다. `skills/herdr/SKILL.md:13,74,145`는 환경 확인·현재 pane 선택·승인 화면에서 사용자 개입을 안내한다 |
| TUI 같은 revision | `src/integration/targets.rs:196`은 SessionStart hook을 설치하고, `src/integration/config_edit.rs:801`은 hooks 기능을 켠다. `src/integration/assets/codex/herdr-agent-state.sh:20,62,86`은 환경·스레드 ID를 확인하고 Python AF_UNIX로 세션 정보를 보낸다. 이 hook은 sandbox 안의 임의 도구 명령에 소켓 접근을 허용한다는 근거가 아니다 |

조사한 실행·환경·hook 경로에서는 Codex의 환경 필터나 도구 sandbox를 위한 추가 예외 설정을 찾지 못했다. App/Core의 `--no-daemon` 결정과 TUI의 별도 hook 방식을 구분해야 한다.

설치 바이너리: `/opt/homebrew/Caskroom/codex/0.167.0/bin/codex`, SHA-256 `030316ecfbb41f4e7946e584b0063ef333b7a36cd312ba349f2f6869e39b62cc`.

| 확인 명령·출력 | Runwick 적용 |
| --- | --- |
| `codex --help`, `codex resume --help`: `--no-daemon` 설명은 “Run without the shared background server”. `codex --no-daemon -c 'developer_instructions="probe"' resume <UUID> --help` 종료 코드 0 | 전역 옵션을 `resume <id>` 앞에 둔다 |
| 임시 폴더의 `codex -c 'developer_instructions="RUNWICK_DEVELOPER_PROBE"' debug prompt-input probe`: 해당 문자열이 `role: developer`에 나타남 | 협업 안내를 켠 새 세션·재개에 주입한다. 이는 해당 키의 기존 값을 실행 동안 덮어쓰는 override다 |
| `codex app-server --strict-config --print-startup-config`에 `shell_environment_policy.inherit="core"` 및 `set.RUNWICK_HOME`, `set.RUNWICK_SOCKET`, `set.RUNWICK_SESSION_ID` 문자열을 전달: 종료 코드 0. 정수 값은 `expected a string`으로 거절 | 공개 값만 전달하고 토큰·PATH는 override하지 않는다. CLI 절대 경로를 안내한다. 실제 도구 셸에서 필터 이후 값 전달은 후속 검증 대상이다 |
| `codex sandbox --help`: `--allow-unix-socket <path>` 지원. 일반 `codex --allow-unix-socket ... --help`는 거절. strict config에서 `network.allow_unix_sockets`와 `sandbox_workspace_write.allow_unix_sockets`도 거절 | 일반 세션에 이 옵션을 그대로 붙일 수 없다 |
| strict config에서 `permissions.<name>.network.unix_sockets={"/tmp/runwick-probe.sock"="allow"}`와 `default_permissions=<name>`은 통과. 기본 프로필 선택이 없으면 거절; `extends="workspace-write"`는 undefined profile로 거절. 바이너리 `strings`에도 Unix 소켓용 Seatbelt bind/outbound 규칙이 있음 | 단일 소켓 기능은 있으나 사용자의 기존 권한 프로필을 보존하는 일반 적용 방법은 미확인이다. 프로필을 교체하지 않고 Codex의 권한 승격·화면 승인을 안내한다. 중첩 Seatbelt 실행은 이 개발 sandbox에서 `Operation not permitted`로 차단됐다 |

Runwick 기준 revision `d188712395f0c5d67a5046ff7b89d90c9b3b431b`의 `crates/runwick-host/src/collab/identity.rs:37,68`은 알려진 peer의 프로세스 계보를 우선한다. 이 규칙과 window 전용 경계는 변경하지 않는다. `crates/runwick/src/agent.rs:219`가 세션 ID도 요구하므로 공개 환경값에 포함했다. `crates/runwick-host/src/sessions/codex.rs:47`은 `originator`를 검사하지 않고 `source="exec"`만 제외하므로 CLI·app-server 변형을 합성 테스트로 확인했다. 실제 `--no-daemon` rollout 연결·재개, 부모/작업자 왕복, MCP 자식 종료는 coordinator가 확인해야 한다.

## 소스 근거

- [H1] [App AGENTS 문서](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/defaults/AGENTS.md)
- [H2] [worktree mod](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/defaults/mods/worktree/main.js)
- [H3] [Core holder](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/src/holder.rs), [keep tests](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/tests/keep.rs)
- [H4] [App model](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/src/model.rs), [Core terminals](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/src/terminals.rs)
- [H5] [App terminal](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/src/terminal.rs), [VT renderer](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/src/vt.rs)
- [H6] [Changes mod](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/defaults/mods/changes/main.js)
- [H7] [Core compatibility tests](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/tests/compat.rs)
