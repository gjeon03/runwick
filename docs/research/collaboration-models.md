# Pinloom·Herdr의 협업 구조와 사용성 비교

조사일: 2026-11-05 · 상태: **소스 확인과 설계 판단을 구분한 조사 기록**

이번 조사는 팀 생성·작업자 생성·전달·상태·사용자 개입·종료 경로를 따라 읽었다. 아래 소스 범위에 대한 분석이며 모든 기능을 전부 검증했다는 뜻은 아니다. 기존 Herdr 실행 실험의 범위와 한계는 [Herdr 조사](herdr.md)에 있다. 이번 추가 조사에서는 실제 AI 작업이나 사용자 데이터 접근을 실행하지 않았다.

## 결론

Herdr에서 배울 핵심은 **팀이라는 사전 설정 없이, 현재 사용하는 에이전트가 필요한 실행을 만들고 사람이 언제든 들여다볼 수 있다는 점**이다. Runwick도 기존 CLI 한 세션에서 출발하도록 권장한다. 협업 관계·작업 기록은 실제 위임이 발생할 때 생기며, 사용자가 팀 이름·멤버·역할·계획부터 입력하지 않는다.

Pinloom도 AI가 동적으로 작업자를 만들 수 있다. 두 제품의 차이를 ‘수동 팀 대 자동 팀’이라고 설명하면 부정확하다. 차이는 **팀 소속을 먼저 요구하는 실행 계약인지, 실행 자체를 조합하는 계약인지**에 있다.

## 확인한 코드 경로

| 근거 | 파일·심볼 | 확인한 사실 | 사용성에 대한 해석 |
| --- | --- | --- | --- |
| P1 | `services/teams.ts`: `createTeam`, `assertSessionFree`, `createWorker` | 팀마다 대표 세션 하나, 세션의 팀 소속 제약, 팀 내부 alias 유일성. MCP가 Claude/Codex 작업자를 생성할 수 있고 첫 배정 때 실행 | 역할 재사용·호출 대상이 명확하지만, 일회성 협업도 팀 구조를 알아야 함 |
| P2 | `mcp-server/src/index.ts`: 시작 검증, `team_create_worker`, `team_ask`, `team_read` | team ID/token 없이 MCP shim이 시작되지 않음. 타 프로젝트 작업자 생성, 태그 전송, 결과 핸들 조회 지원 | 교차 프로젝트 협업과 영속 결과 핸들은 살릴 가치가 있음. 팀을 권한·주소·작업 묶음으로 동시에 사용함 |
| P3 | `services/dispatches.ts`: `createDispatch`, `getLiveDispatchForWorker` | dispatch 영속 상태와 worker당 진행 중 예약. 새 dispatch가 이전 진행 행을 superseded/cancelled로 바꾸는 경로 | 작업 요청의 식별자는 필요하지만, 바쁜 세션에 새 요청을 했다고 이전 작업을 자동 대체하는 UX는 재검토 |
| P4 | `routes/team-dispatch.ts`: `/ask` | terminal 경로는 TUI 전달 후 transcript에서 결과 확정. SDK 경로는 대기 만료 후에도 dispatch 핸들로 조회 가능 | 전달·응답·대기 만료를 분리하는 장점. SDK/terminal 간 동일 동작은 별도 검증 필요 |
| H1 | Core `api/run.rs`: `terminal_run`, `start`, `terminal_env` | `caller`에서 부모를 정하고 `HC_TERMINAL_ID` 주입. 팀 ID가 아닌 terminal 주소로 실행. template는 선택 사항 | 평소 CLI에서 위임을 시작할 수 있음. 고정 팀을 만들 필요가 없음 |
| H2 | Core `api.rs`: `terminal_send`; `input.rs`: `send_message` | terminal 주소로 전송. 사용자 입력과 순서를 조율하고 blocked 상태에서는 기본적으로 거부. turn/status와 시작 여부 반환 | 자유로운 실행도 입력 경합·승인 화면 오입력에 대한 보호가 필요함 |
| H3 | Core `notes.rs`: `deliver_notes`, `type_note`; `status.rs`: `apply` | 상태 보고와 부모 알림. 알림은 전달 시점에 구성하고 입력 가능 여부 재확인. 상태는 프로그램이 보고 | 에이전트가 통신과 판단을 담당하고 런타임은 실제 관측·전달을 담당 |
| H4 | Core `src/SKILL.md`, `docs/templates.md` | CLI용 사용 설명, 역할별 실행 template, 먼저 작업을 전달했는지 확인하는 지침. `hc wait` 만료는 작업 실패가 아닌 재확인 시점 | 앱의 기능을 AI도 배울 수 있어야 함. 부팅 지연·전송 불확실성을 중복 실행으로 해결하면 안 됨 |
| H5 | App `defaults/mods/agents/main.js`: `tree`, `kidStatus`, `render` | 자식은 필요할 때 펼치고 peek. 작업자의 긴급 상태를 대표 항목 정렬에 반영. 자식 done은 자체 새 결과 알림으로 취급하지 않는 경로 | 평소에는 대표 일만 보고 필요할 때 세부를 여는 방식이 좋음 |
| H6 | App `commands.rs`, `window.rs` | peek, keep, park를 분리. 잠깐 보기와 정식 탭으로 두기를 구분 | 도움 작업자마다 중앙 탭을 강제 생성하지 않아도 관찰·직접 개입 가능 |
| H7 | App `defaults/mods/worktree/main.js`: `create` | branch/worktree 생성 후 `space.new`로 폴더·프로그램 연결 | 분리를 하나의 행동으로 제공. 이 경로에 의존성 설치·포트·`.env` 준비까지 포함된 것은 아님 |
| H8 | App `model.rs`: `space_json` | 공간·탭 rollup은 layout pane 주소를 사용. H5의 Agents 목록은 자식까지 별도로 계산 | 같은 실행에 대한 상단 배지와 작업자 목록의 집계 범위를 통일해야 함 |
| H9 | Core `README.md`, `terminals.rs`, `runtime.rs` | 기본 자식 수명은 부모 실행에 연결되며 outlive 옵션 제공. UI/Core/holder 분리. 종료 기록은 제한 보존 | 실행 관계와 영구 작업 기록은 다른 책임. UI 종료·대표 응답 완료·프로세스 종료도 구별 필요 |

`P3`는 실제 코드의 동작을 설명한 것으로, 데이터 손실이나 특정 장애가 재현됐다는 주장은 아니다. `H8` 역시 서로 다른 집계 경로를 확인한 것이며 모든 화면에서 동일한 버그를 재현한 것은 아니다. 기존 설계 문서에 ‘미구현’이라고 적혀 있어도 dispatch 코드는 존재하므로 문서만으로 구현 유무를 판단하지 않았다.

## 사용 흐름 비교

| 장면 | Pinloom의 확인된 구조 | Herdr의 확인된 구조 | Runwick 권장안 |
| --- | --- | --- | --- |
| 혼자 작업 시작 | 프로젝트·세션 | space/tab에서 CLI 실행 | 기본 공간의 CLI로 바로 시작 |
| 검토자 한 명 필요 | 팀 안에서 worker를 추가하거나 생성 | 기존 agent가 `hc run`으로 실행 | 현재 CLI가 위임. 협업 내역 자동 생성 |
| 같은 사람에게 후속 요청 | alias와 멤버 유지 | 실행 주소나 template로 접근 | 같은 세션 재사용, 바쁘면 대기. 멤버 등록 없음 |
| 여러 저장소 작업 | 팀의 다른 프로젝트 worker | 각 실행 cwd 또는 원격 machine | 원래 프로젝트와 대상 프로젝트를 명시한 연결. 전역 활동에서 함께 확인 |
| 잠깐 작업자 보기 | 실제 worker 세션 열기 | peek 후 필요하면 keep | 미리보기 → 직접 대화 → 탭에 유지 |
| 사용자 승인 필요 | transport별 상태·전달 | 프로그램 상태와 blocked 보호 | 현재 요청으로 이동. 식별 가능한 승인만 해당 요청에 응답 |
| 작업 후 기록 찾기 | 세션·메시지·핀·위키·타임라인 | terminal 출력·제한된 종료 기록 | 실행 종료와 독립된 대화·핀·위키·결과 출처 |
| 기본 작업 방식 바꾸기 | 팀·멤버 지시문 | agent 설명·template·mod | 선택적 역할/작업 방식 재사용. 고정 인원·단계 강제 없음 |

## 세 가지 설계안

| 안 | 이점 | 부담 | 판단 |
| --- | --- | --- | --- |
| A. 팀을 먼저 만들고 배정 | 역할과 장기 구성 관리가 명시적 | 팀·대표·멤버 선언이 일회성 작업에도 필요 | 반복 조직 작업의 선택 기능으로만 적합 |
| B. terminal 실행 관계만 제공 | 시작이 자연스럽고 CLI 자유도가 높음 | 장기 기록·작업별 재시도·결과 출처는 별도 설계 필요 | 실행 기반으로 채택할 장점이 큼 |
| C. 세션에서 협업이 생기고 기록이 붙음 | B의 시작 흐름에 Pinloom의 기억·검색을 결합 | 실행과 대화와 작업 식별을 분리해야 함 | **추천. 팀 생성도 목표 등록도 필수 아님** |

C에서 작업 기록은 사용자에게 또 다른 관리 양식을 요구하는 것이 아니다. 단순 대화에는 세션만 있고, 위임·결과 연결이 생겼을 때 런타임이 요청과 관계를 기록한다. AI가 명시한 단계만 표시하며 모든 답변에 가짜 ‘분석 → 작성 → 검토’ 워크플로우를 붙이지 않는다.

## 가져올 제약과 없앨 제약

| 제약 | 권장 처리 | 이유 |
| --- | --- | --- |
| 먼저 팀·대표·alias 선언 | 제거 | 시작하려는 일보다 조직 구성이 앞서지 않게 함 |
| 세션이 한 팀에만 소속 | 제거 | 이력·자료 공유는 여러 작업과 연결 가능 |
| 한 CLI 입력에 여러 주체가 동시 전송 | 직렬화 유지 | 사람 입력과 위임이 섞이는 실제 실행 충돌 방지 |
| worker 생성 수 16 같은 고정 제품 상한 | 사용자에게 보이는 자원 정책으로 대체 | 사용량에 맞춰 조절. AI도 잔여 한도를 조회 |
| 모든 세션에 worktree 강제 | 도입하지 않음 | 읽기·검토·순차 작업은 같은 공간으로 충분 |
| 모든 질문에 계획/승인/검토 단계 | 도입하지 않음 | 작은 요청의 완료까지 관리 절차가 방해하지 않음 |
| 보고되지 않은 상태를 추정해 완료 | 금지 | 응답 완료·작업 완료·검증 통과의 혼동 방지 |
| 재개 불확실한 실행을 새로 만들어 대체 | 금지 | 기존 대화·부작용·결과의 연속성 보존 |
| 자식이 자식을 무한 생성 | 공통 자원 한도·취소 범위 안에서만 허용 | 깊은 협업을 일률적으로 막지 않으면서 실행 폭증 방지 |

Herdr도 아무 제약이 없는 것은 아니다. 실제 blocked 입력 보호가 있고, agent 안내에는 재위임 범위를 명시하라고 적혀 있다. 다만 **안내문에 적힌 규칙과 런타임이 강제하는 조건은 구분**해야 한다. Runwick의 실행 한도도 런타임을 경유한 작업에 대해서만 보장할 수 있으며, 일반 셸에서 실행한 외부 프로세스까지 통제한다고 표현하지 않는다.

## 이번 분석에서 확정하지 않은 것

- Herdr 전체 코드 감사, 실제 Claude/Codex 다중 에이전트의 성공률·비용·배터리 비교.
- Pinloom에서 과거 겪은 모든 장애의 원인. 현재 main과 과거 실험 브랜치가 같다고 가정하지 않음.
- Herdr Core를 직접 채택할지, 아이디어를 참고해 별도 코어를 만들지. API 적합성·라이선스·의존성·유지보수 검증 후 결정.
- 모든 provider의 내부 하위 에이전트를 동일 수준으로 관찰·중지할 수 있다는 가정. 미지원 상태를 제품에 명시해야 함.

## 소스

공식 설명: [Core](https://herdr.dev/core/), [사용 가이드](https://herdr.dev/guide/). 문서 설명과 대조한 코드는 아래 revision으로 고정했다.

- P1: [teams.ts](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/teams.ts)
- P2: [MCP 도구](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/mcp-server/src/index.ts)
- P3: [dispatches.ts](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/dispatches.ts)
- P4: [team-dispatch.ts](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/routes/team-dispatch.ts)
- H1: [run.rs](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/src/api/run.rs)
- H2: [api.rs](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/src/api.rs), [input.rs](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/src/input.rs)
- H3: [notes.rs](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/src/notes.rs), [status.rs](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/src/status.rs)
- H4: [agent 안내](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/src/SKILL.md), [templates](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/docs/templates.md)
- H5: [Agents mod](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/defaults/mods/agents/main.js)
- H6: [commands.rs](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/src/commands.rs), [window.rs](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/src/window.rs)
- H7: [Worktree mod](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/defaults/mods/worktree/main.js)
- H8: [model.rs](https://github.com/herdrdev/herdr-app/blob/e77f112ac2ea136b84fe510efa42d41163bfd776/src/model.rs)
- H9: [Core README](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/README.md), [terminals.rs](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/src/terminals.rs), [runtime.rs](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/crates/herdr-core/src/runtime.rs)
