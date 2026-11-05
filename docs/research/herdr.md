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

## 소스 근거

- [H1] [App AGENTS 문서](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/defaults/AGENTS.md)
- [H2] [worktree mod](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/defaults/mods/worktree/main.js)
- [H3] [Core holder](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/src/holder.rs), [keep tests](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/tests/keep.rs)
- [H4] [App model](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/src/model.rs), [Core terminals](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/src/terminals.rs)
- [H5] [App terminal](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/src/terminal.rs), [VT renderer](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/src/vt.rs)
- [H6] [Changes mod](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/defaults/mods/changes/main.js)
- [H7] [Core compatibility tests](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/tests/compat.rs)
