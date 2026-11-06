# Orca·Tailscale 원격 작업 검토

조사일: 2026-11-06 · 상태: **여러 기기 실행·공간 전환 방향 합의 · 상세 정책/기술 미확정 · 미구현**

조사 이후 사용자는 ‘Herdr처럼 각 기기의 실행을 유지하고, 간단히 기기를 등록하여 기존 사이드바에서 원격 공간을 전환’하는 방향에 동의했다. 아래의 구체적인 연결·설정·기록·모바일 정책까지 모두 승인한 것은 아니다. 합의 범위는 [결정 기록](../decisions.md)을 따른다.

## 1. 대상 확인과 결론

사용자가 말한 ‘Orca에서 Tailscale로 원격 작업 전환’은 **[stablyai/orca](https://github.com/stablyai/orca)**의 기능과 일치한다. 이름이 비슷한 다른 Orca 제품과 구분했다. 공식 Remote Orca Servers 문서는 양쪽 기기에 Orca와 Tailscale을 설치하고 접근 링크로 연결하는 경로를 권장한다. 인기도나 이용자 전체의 선호를 측정한 것은 아니다. [O1][O2]

**Runwick에도 도입할 가치가 높다.** 장시간 작업은 항상 켜진 컴퓨터에서 실행하고, 맥북에서는 같은 작업을 확인·조작하는 흐름이 기존의 가벼움·세션 지속·기억이라는 목표와 잘 맞는다. 원격 기능을 기획 검토 대상으로 올리고, 코어의 실행 위치·식별자·파일/기록 접근 경계는 처음부터 고려하는 것을 추천한다. 출시 포함 여부와 네트워크 구현 방식은 아직 결정하지 않는다.

## 2. 실제로 제공하는 경험

| 방식 | 실행과 파일의 위치 | 사용자의 경험 |
| --- | --- | --- |
| Orca SSH worktrees | SSH 대상 컴퓨터 | 로컬 UI에서 대상 기기를 골라 worktree·에이전트 실행, 파일·diff 사용 [O3] |
| Remote Orca Server | Orca가 실행 중인 다른 컴퓨터 | 한 번 연결한 서버의 프로젝트·세션·탭에 접속. 클라이언트가 끊겨도 서버 실행 유지 [O2] |
| Orca Mobile | 페어링한 desktop/server가 기준 | 휴대폰에서 상태·최근 출력·질문 응답 등 사용. 경로는 LAN/Tailscale/relay 등으로 구분 [O4] |
| Herdr Core machines | 각 컴퓨터의 Core | SSH로 등록한 기기의 실행·이벤트를 함께 조회하고 원격 파일/terminal 접근 [H1] |

Orca의 원격 서버 연결은 ‘내 컴퓨터에서 돌던 프로세스를 다른 컴퓨터로 옮긴다’는 뜻이 아니다. 서버가 실행과 파일을 소유하고 다른 클라이언트가 그 서버에 접속한다. 로컬 파일의 수정사항·인증·환경변수·실행 중 메모리를 Tailscale이 자동으로 복제하는 것으로 이해하면 안 된다.

공식 문서는 서버에 CLI·저장소·도구·인증이 필요하고 서버가 깨어 있어야 한다고 설명한다. 노트북이 잠드는 것과 서버가 잠드는 것은 다르다. 원격 서버 기능은 조사 당시 beta다. [O2]

## 3. Tailscale이 맡는 부분

Tailscale은 기기 사이의 사설 네트워크 연결을 제공한다. Runwick은 이를 통해 SSH 또는 자체 인증된 런타임 연결을 사용할 수 있다. 기기 이름/주소로 접근하는 경로와 앱의 세션·기록·재연결 기능은 별도 책임이다.

- Tailscale 네트워크 위에서 일반 OpenSSH를 사용하는 것과 ‘Tailscale SSH’ 기능은 다르다. macOS에서는 Tailscale SSH의 설치 형태별 제약이 있으므로 일반 SSH 경로도 고려한다. [T1]
- 직접 연결이 가능한 경우와 중계를 사용하는 경우가 있으며 지연·처리량은 달라질 수 있다. 원격 터미널이 로컬과 항상 같은 속도라고 약속하지 않는다. [T2]
- 브라우저 접근을 추가한다면 Serve는 tailnet 내 공유, Funnel은 공개 접근이라는 차이가 있다. 개인 작업실의 기본 경로로 Funnel 공개를 요구할 이유는 없다. [T3][T4]
- 같은 tailnet의 기기라는 사실과 Runwick을 조작할 권한은 분리한다. 기기별 등록·인증·해제와 실제 대상 기기 확인이 필요하다. Orca도 클라이언트별 철회 가능한 권한을 두고 있다. [O2]

Tailscale을 쓰지 않는 LAN·기존 SSH 환경도 지원 가능한 경계로 두는 것이 좋다. 자체 VPN이나 범용 원격 데스크톱 프로토콜을 새로 만드는 것이 이번 목표는 아니다.

## 4. 읽은 코드와 배울 점

Orca는 GitHub main의 **`53fbfded41b8b0462d7ccd029475f89d12aa9bf5`**를 고정해 아래 경로를 읽었다. 원격 관련 전체 코드 감사나 실제 2기기 실행 시험을 수행한 것은 아니다. 공식 배포 문서와 해당 소스의 일부 메뉴 명칭은 다를 수 있다.

| 소스 | 확인한 동작 | Runwick에 대한 판단 |
| --- | --- | --- |
| `remote-runtime-tailscale-hint.ts` | Tailscale 주소 형태를 식별해 연결 실패 안내 추가. 인증/프로토콜 오류는 일반 연결 오류로 바꾸지 않음 | ‘원격 연결 실패’ 하나로 뭉개지 말고 네트워크·인증·버전·앱 미실행 구분 |
| `remote-runtime-shared-control-reconnect.ts` | 활성/유휴 연결의 재시도 간격 구분, 수동 연결 해제 존중, 복귀 시 중복 재접속 방지 | 배터리와 끊김 복구를 함께 고려. 무한 빠른 polling 금지 |
| `host-route-call.ts` | 작업을 지정 호스트로 보내고 응답 전 호스트 식별이 바뀌면 unverifiable 처리. 자동 재적용/재시도하지 않음 | 잘못된 기기에 파일 수정·명령이 도달하는 문제와 중복 실행 방지 |
| `runtime-host-pairing-identity.ts` | host key와 installation identity로 재연결 대상 일치 판단 | 주소/표시명만 같다고 같은 서버로 취급하지 않음 |
| `runtime-environment-authority-binding.ts` | SSH 접근 상태를 페어링·endpoint 정보와 연결, 재시작마다 바뀌는 runtime ID와 구분 | 기기/설치 ID, 실행 세대, 접속 주소의 역할을 분리 |

Herdr의 [machines 문서][H1]에서도 원격 Core가 실행·기록·파일을 소유하고 로컬 Core는 연결과 조회를 결합한다. 연결 등록 해제가 원격 terminal 종료를 뜻하지 않는다는 점도 명시한다. 기존 Runwick의 창 닫기/실행 종료 구분과 잘 맞는다.

## 5. Runwick에 적용할 세 가지 경로

| 선택 | 장점 | 부담 | 판단 |
| --- | --- | --- | --- |
| A. 원격 SSH 셸만 제공 | 간단히 연결 가능 | 원격 파일·핀·위키·세션 수명은 별도 경험으로 남기 쉬움 | 연결 확인용 실험으로는 유용, 제품 목표로는 부족 |
| B. 원격 Runwick Core + 같은 네이티브 GUI | 기존 터미널/파일/기록 경험을 실행 위치와 함께 유지 | 원격 인증·프로토콜·재연결·기록 소유권 설계 필요 | **제품 목표로 추천** |
| C. B와 모바일/브라우저 전체 UI 동시 구축 | 외부에서 접근하기 편함 | 화면·입력·배포 범위가 크게 늘어남 | 모바일은 확인/응답/중지 중심의 별도 확장으로 검토 |

B에서 Tailscale은 추천 연결 경로이며 필수 브랜드 종속 요소로 만들지 않는다. 초기 기술 실험은 **설치된 Tailscale + 표준 SSH로 원격 Rust Core에 연결**하는 방식이 타당하다. 키·호스트 확인 등 기존 체계를 활용해 세션 지속과 양방향 통신부터 검증할 수 있다. 사용자가 경험할 목표는 ‘기기 한 번 등록 → 공간 선택’이다. Orca처럼 링크/QR 페어링을 제공할지는 동일 코어 위의 별도 연결 방식으로 평가한다.

이 제안은 Rust 기반 네이티브 GUI와 양립한다. Tailscale은 OS에 설치된 외부 도구를 활용하고, Runwick의 데스크톱 UI를 Electron이나 원격 화면 스트리밍으로 바꿀 필요가 없다.

## 6. 사용 흐름 제안

1. **기기 한 번 등록:** 이 Mac 외에 `집 Mac mini`, `개발 서버`를 이름 붙여 연결한다. 도달 가능·인증·Core 버전·설치된 CLI를 확인한다. 기기를 추가해도 기존 프로젝트의 실행 위치를 자동 변경하지 않는다.
2. **원격 폴더 열기:** 기기에서 실제 저장소 경로를 선택한다. 기존 프로젝트와 같은 제품이면 사용자가 연결 관계를 확인하고 해당 프로젝트의 원격 공간으로 표시한다. Git URL이 같다는 이유만으로 자동 병합하지 않는다.
3. **원격 세션 시작:** 공간의 `Mac mini` 표식을 보면서 평소처럼 CLI를 쓴다. 파일 트리·문서·diff도 그 원격 공간을 가리킨다.
4. **기기/공간 전환:** 로컬과 원격 공간을 같은 사이드바에서 고른다. 원격을 선택했다고 로컬 세션이 종료되거나 다른 기기로 이동하지 않는다. 공간별 탭·문서·스크롤은 보존한다.
5. **연결 끊김·재접속:** 원격 실행이 살아 있으면 같은 실행에 연결한다. 단절 동안 실행 여부를 확인할 수 없으면 ‘마지막 확인 시각’을 표시한다. 미전달 입력을 재접속 뒤 임의로 보내지 않는다.
6. **다른 기기에서 이어보기:** 같은 서버의 세션을 열어 기록·현재 요청·결과를 확인한다. 여러 기기가 보고 있으면 직접 입력권은 한 기기에만 주고 전환 시 기존 입력과 요청을 대조한다.

예시: Mac mini에서 Claude 구현과 Codex 검토를 실행 → 맥북을 닫고 이동 → 다른 연결된 기기에서 같은 작업의 질문에 답변 → 나중에 맥북에서 결과와 핀 확인. 서버 자체가 꺼졌다면 기록 열람/지원되는 대화 재개로 전환한다.

## 7. 기존 기획과 맞춰야 할 계약

| 항목 | 추천 정책 |
| --- | --- |
| 탐색 구조 | 프로젝트 → 작업 공간 → 세션 유지. 각 공간에 실행 기기와 실제 경로를 결합. 기기 필터는 추가 가능하지만 필수 상위 폴더가 아님 |
| 식별자 | 호스트/설치, 공간, 세션, 실행 세대, 요청 ID를 구분. 같은 이름·경로·숫자 ID로 원격과 로컬을 혼동하지 않음 |
| 실행/기록 | 실행 호스트가 세션·원문·결과의 기준. UI는 조회/캐시. 같은 SQLite 파일을 네트워크로 공유해 수정하지 않음 |
| 검색·핀·위키 | 원문/핀의 소유 호스트·프로젝트를 보존. 여러 기기에서 수정은 소유 호스트로 전송. 전역 검색은 연결된 호스트와 승인된 로컬 캐시를 결합하고 검색 누락 범위를 표시 |
| 오프라인 | 내려받은 기록만 읽고 마지막 동기화 시각 표시. 미수신 기록을 없는 기록으로 단정하지 않음. offline 수정은 초안이며 재연결 시 버전 대조 후 저장 |
| 직접 입력/파일 편집 | 원격 호스트가 입력권과 파일 버전을 확인. 두 클라이언트의 입력/저장이 서로 덮어쓰지 않게 함. 끊긴 입력권의 재획득도 현재 요청 기준 |
| 공통 환경변수 | 기존 ‘공통 하나’ 요구 유지. 관리할 공통 값 집합은 하나로 두고 기기별 적용 버전/미적용 상태를 표시하는 안. 원격 비밀값 전달은 허용한 기기에만 명시적으로 설정. HOST/PATH 등 OS 고유 환경을 통째로 동기화하지 않음 |
| provider 인증/MCP | 인증과 설정 경로는 실행 호스트 기준. 노트북 로그인·로컬 MCP 경로를 원격에 자동 복제하지 않음. 원격 대화 새로고침은 그 호스트의 정확한 대화 ID에 수행 |
| 자동 분석/배터리 | 실행 호스트의 전원·idle·동시 배정을 기준으로 판단. 클라이언트 맥북의 배터리 전환이 전원 연결된 서버의 작업을 자동 중지하지 않음. 지식의 소유 위치와 분석 담당을 정해 두 기기가 같은 정리를 중복 실행하지 않음 |
| 미리보기 서버 | 원격 `localhost`와 로컬 `localhost`를 구분. 선택한 개발 포트만 연결하고 연결 종료/포트 충돌을 표시. 자동 공개 인터넷 노출 없음 |
| 종료 | ‘연결 끊기’는 원격 실행 유지. ‘원격 작업 종료’는 대상 기기와 실행 영향 표시. 연락이 끊기면 종료 요청 여부와 실제 확인을 구별 |
| 이동 | 이미 실행 중인 로컬 세션을 다른 기기로 이동하는 것은 별도 인계 작업. Git 변경·문서·인증·provider 기록의 가용성을 확인하고 새 위치에서 재개. Tailscale 연결만으로 수행하지 않음 |

공통 환경의 원격 적용 방식과 지식의 소유 위치는 이번에 새로 생긴 검토 항목이다. 위 정책을 확정된 기존 요구사항처럼 소급하지 않는다. 실행 기기별로 서로 다른 프로젝트/실행 환경 프로필을 늘리는 방향은 피한다.

## 8. UI 기획에 추가될 것

- S01/S02: 공간·세션의 실행 기기 표식, 로컬/원격 필터, 연결 끊김·마지막 확인 시각. 전환은 해당 공간만 변경.
- S03/S11: 원격 파일 출처, 지연/저장 대기/충돌, 해당 호스트에서 실행한 검증 결과. 경로 복사 시 기기를 식별할 수 있는 링크와 실제 경로 구분.
- S06: 기기별 활동·관측 가능한 자원·입력 요청·연결 상태. 네트워크 단절을 AI 작업 실패와 구분.
- S07~S09: 원격 출처, 검색된 호스트/캐시 범위, 소유 호스트 연결이 필요한 수정 표시.
- S12/S13: 등록 기기·연결 확인·권한 해제, 도구/인증/환경 적용 상태, 원격 세션 새로고침.
- S14: 백업 대상 호스트와 실제 저장 위치 표시. 로컬 캐시를 전체 원격 백업으로 표현하지 않음.
- 추가 연결 화면: 기기 등록 → 도달/인증 확인 → Core 호환성 → 원격 폴더 선택 → 사용 가능. 기술 실패는 해당 단계에서 재시도하며 로컬 기본 공간으로 대체 실행하지 않음.

모바일은 데스크톱 화면 전체를 축소하기보다 ‘확인할 일 → 질문 응답 → 결과/기록 → 중지’ 흐름을 먼저 검토한다. 여러 기기 실행 방향을 채택했으므로 UI 디자인에는 원격 공간과 연결 상태를 기존 화면과 함께 반영한다. 모바일 UI 채택은 별도 판단이다.

## 9. 도입 전 검증 과제

1. 같은 원격 프로세스가 클라이언트 종료·Wi-Fi 변경·Tailscale 재연결 뒤에도 유지되고, 단절 중 명령이 중복 실행되지 않는지.
2. 두 클라이언트가 같은 세션을 열었을 때 입력권, terminal resize, 파일 저장 충돌이 처리되는지. 단순 화면 전환이 다른 기기의 포커스를 빼앗지 않는지.
3. Tailscale 직접 연결/중계 각각의 입력 지연·대량 출력·유휴 CPU/네트워크·맥북 배터리. Rust나 원격이라는 이유만으로 절감률을 주장하지 않음.
4. 원격 호스트 재부팅, Core 버전 불일치, 접근 권한 해제, 같은 주소의 다른 설치, 원격 종료 확인 실패의 UI/복구.
5. 실제 Claude/Codex의 원문 수집·같은 대화 재개·MCP/환경 새로고침. 로그인 이전이 자동이라고 가정하지 않음.
6. 원격 파일·미커밋 변경·위키/핀·백업의 소유권, 검색 누락 표시, 재연결 후 동기화 중복 방지.

현재는 공식 문서와 지정 코드 경로를 확인하고 제품 방향에 합의한 단계다. Tailscale 설치·기기 등록·원격 포트 노출·사용자 인증 복사·실제 원격 실행은 수행하지 않았다. 다음 설계에서는 구체적인 연결 명세와 2기기 검증 과제를 정한다.

## 근거

- [O1] [Orca 저장소](https://github.com/stablyai/orca), [실행 방식](https://www.onorca.dev/docs/ways-to-run)
- [O2] [Remote Orca Servers](https://www.onorca.dev/docs/remote-servers), [고정 revision 문서](https://github.com/stablyai/orca/blob/53fbfded41b8b0462d7ccd029475f89d12aa9bf5/docs/site/content/docs/remote-servers.mdx)
- [O3] [SSH worktrees](https://www.onorca.dev/docs/ssh)
- [O4] [Mobile companion](https://www.onorca.dev/docs/mobile)
- [C1] [Tailscale 오류 안내](https://github.com/stablyai/orca/blob/53fbfded41b8b0462d7ccd029475f89d12aa9bf5/src/shared/remote-runtime/remote-runtime-tailscale-hint.ts)
- [C2] [연결 재시도](https://github.com/stablyai/orca/blob/53fbfded41b8b0462d7ccd029475f89d12aa9bf5/src/shared/remote-runtime/control/remote-runtime-shared-control-reconnect.ts)
- [C3] [호스트 지정 호출](https://github.com/stablyai/orca/blob/53fbfded41b8b0462d7ccd029475f89d12aa9bf5/src/renderer/src/runtime/host-route-call.ts)
- [C4] [페어링 identity](https://github.com/stablyai/orca/blob/53fbfded41b8b0462d7ccd029475f89d12aa9bf5/src/shared/remote-runtime/host-status/runtime-host-pairing-identity.ts), [SSH 접근 binding](https://github.com/stablyai/orca/blob/53fbfded41b8b0462d7ccd029475f89d12aa9bf5/src/shared/remote-runtime/host-catalog/runtime-environment-authority-binding.ts)
- [H1] [Herdr Core machines](https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/docs/machines.md)
- [T1] [Tailscale SSH](https://tailscale.com/docs/features/tailscale-ssh)
- [T2] [Tailscale connection types](https://tailscale.com/docs/reference/connection-types)
- [T3] [Tailscale Serve](https://tailscale.com/docs/features/tailscale-serve)
- [T4] [Tailscale Funnel](https://tailscale.com/docs/features/tailscale-funnel)

[H1]: https://github.com/herdrdev/herdr-core/blob/c6b316f87c33e6f2ab1a1ceea3e3461e2b6bb61a/docs/machines.md
