# Pinloom에서 살릴 경험과 데이터 검토

조사일: 2026-11-05 · 소스 기준: [`e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39`](https://github.com/gjeon03/pinloom/tree/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39)

현재 main의 소스와 문서를 읽은 결과다. 실제 사용자 DB·환경변수 값·위키 본문은 읽지 않았다. 아래의 개선 방향은 Runwick에 대한 제안이다.

## 우선 보존할 경험

| 기능 | 확인한 구현 | Runwick에서 살릴 점과 개선 후보 |
| --- | --- | --- |
| 환경변수 관리 | `user_env` SQLite 저장, 저장·시작 시 `process.env` 반영, 자식 프로세스 환경 정리 [P1] | 한 번 설정해 여러 도구에서 사용. 전역·프로젝트·실행 범위와 값의 출처를 명확히 하고 비밀값 저장 방식을 재설계 |
| 영속 세션 기록 | 자체 `sessions`·`messages`, provider 세션 ID, Claude/Codex PTY transcript 수집 [P2] | CLI 기록이 없어져도 이미 수집한 대화를 검색. 실시간 터미널 버퍼와 영구 대화 아카이브를 분리 |
| 세션 위키 | 프로젝트 범위 Markdown, 세션→위키 변경 제안, 자동 분석·동기화 조건 [P3] | 지식을 원문 세션과 연결하고 사람이 검토한 내용을 재사용 |
| 타임라인 | 프로젝트·날짜별 Markdown, idle 세션과 Git 기록을 요약, 저장 성공 후 cursor 진행 [P4] | 언제 무엇을 왜 했는지 찾기. 원문 링크와 생성 시점을 보존하고 요약 비용을 드러냄 |
| 핀·즐겨찾기 | 메시지 pin·제목·시각, 프로젝트 favorite [P5] | 다시 찾을 내용을 빠르게 고정하고 프로젝트 접근을 단축 |
| 인수인계·이어하기 | 세션별 날짜 요약을 캐시하고 현재 상태·결정·남은 일을 구성 [P6] | 자동 요약과 함께 짧은 수동 메모를 지원하고 마지막 작업으로 바로 이동 |

“기록을 남긴다 → 중요한 내용을 고른다 → 프로젝트 지식으로 활용한다 → 다음 작업에 연결한다”는 흐름이 Runwick에서도 보존할 핵심 가치다. 모든 Pinloom 화면을 다시 만드는 것은 별도의 목표로 두지 않는다.

## 백그라운드 작업에서 배울 점

코드에는 이미 중복 실행 방지와 실행 간격 제한이 있다. 따라서 현재 배터리 소모 원인을 이 코드만으로 단정할 수 없다. Runwick에서는 동작 제한에 더해 사용자가 상태를 확인하는 경로를 설계할 필요가 있다.

| 작업 | 현재 소스에서 확인한 조건 |
| --- | --- |
| 위키 코드베이스 분석 | 10분 간격 후보 확인, 프로젝트 15분 idle, 신규 메시지 20개 이상, 프로젝트별 최소 24시간 간격, 미검토 제안이 있으면 생략 |
| 세션→위키 동기화 | 10분 간격 후보 확인, 15분 idle, 미동기화 메시지 30개 이상, 시도 간 최소 6시간, 미검토 제안이 있으면 생략 |
| 타임라인 수집 | 1분 간격 확인, 15분 idle, 실행·대기 메시지가 있는 세션 제외, 프로젝트·날짜별 재요약 간격 제한 |
| 검색 인덱싱 | 앱 시작 경로에서 임베딩 준비와 메시지 인덱서 실행. 상세 자원 사용량은 측정하지 않음 |

각 주기는 LLM 호출 주기가 아니라 **후보를 검사하는 주기**다. 조건이 맞는 작업만 실행한다. 위키 자동 결과는 검토 가능한 proposal로 생성한다. 과거 기능 문서의 직접 편집 설명보다 현재 코드 경로를 우선했다. [P3][P4][P7]

## 그대로 옮기기 전에 재설계할 부분

- `is_secret`는 UI 마스킹 등의 표시 설정이며 SQLite의 값을 암호화하지 않는다. Runwick은 환경변수 사용 경험을 유지하면서 OS 자격증명 저장소 등의 방식을 비교한다. 비밀값을 문서·위키·일반 백업에 섞지 않는다. [P1]
- 환경 전달에는 포트와 provider 내부 세션 표식 제거, UTF-8 locale 기본값 같은 실제 호환성 처리가 있다. 단순히 부모 환경을 복사하는 것으로 동일한 경험이 되지 않는다. [P1]
- 저장된 `agent_session_id`만으로 CLI 재개가 보장되지는 않는다. provider의 로컬 기록·설정·버전·작업 경로도 확인해야 한다. [P2]
- 세션 원문 조회 함수는 최근 메시지 중심의 출력 예산을 적용하고 tool 행을 요약 형태로 보여준다. 이 조회 결과를 전체 원본의 완전한 백업으로 취급하면 안 된다. [P2]
- Teams의 구성·통신 경험은 참고하되, Runwick에서는 작업·여러 실행·검토 상태의 관계를 먼저 검토한다. 기존 Teams 전체 이식을 첫 범위로 결정하지 않는다. [P8]

## 데이터 이전에 중요한 발견

현재 `pinloom-db-export` v2는 프로젝트·세션·메시지·핀·프로젝트 favorite 등을 내보낸다. `groupId`를 포함하지만 그룹 레코드 자체는 없다. 환경변수, Teams, 프로젝트 메모, 세션 타임라인 테이블, 각종 cursor·index 상태 등도 이 export에 포함되지 않는다. [P9]

Git 백업의 `wiki-only` v2는 위키와 manifest를 저장하며 DB는 의도적으로 별도 취급한다. 프로젝트 타임라인은 `~/.pinloom/timeline/`에 있으므로 위키 백업만으로 전체 데이터를 이전할 수 없다. 첨부 파일도 별도 목록·경로 검증이 필요하다. [P4][P9]

이 차이를 반영한 [데이터 이전 제안](../data-migration.md)을 별도로 작성했다. 현재 export를 무조건 완전 백업이라고 부르지 않는다.

## 소스 근거

- [P1] [user-env.ts](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/user-env.ts), [child-env.ts](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/child-env.ts), [환경변수 문서](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/docs/features/env-vars.md)
- [P2] [session-transcript.ts](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/session-transcript.ts), [Claude capture](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/claude-pty/transcript-capture.ts), [Codex capture](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/codex-pty/transcript-capture.ts)
- [P3] [wiki-auto.ts](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/wiki-auto.ts), [wiki-session-sync-auto.ts](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/wiki-session-sync-auto.ts)
- [P4] [timeline/capture.ts](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/timeline/capture.ts), [timeline/store.ts](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/timeline/store.ts)
- [P5] [DB migrations](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/db/migrations.ts)
- [P6] [session-handover.ts](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/session-handover.ts)
- [P7] [앱 시작 경로](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/app.ts)
- [P8] [Teams service](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/teams.ts)
- [P9] [db-export.ts](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/db-export.ts), [backup-export.ts](https://github.com/gjeon03/pinloom/blob/e714bef1cdbfebe2c0c3e89e1a6e94ae79e80e39/packages/backend/src/services/backup-export.ts)
