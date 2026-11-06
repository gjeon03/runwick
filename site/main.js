// @ts-check
// Korean remains readable without JavaScript. Only bundled translations become HTML.

/** @typedef {'ko' | 'en'} Language */

/** @type {Record<Language, Record<string, string>>} */
const MESSAGES = {
  "ko": {
    "meta.title": "Runwick — 맥락을 이어 가는 AI CLI 작업실",
    "meta.description": "Claude Code, Codex, 셸을 프로젝트별로. 실행은 이어지고, 대화와 파일은 한곳에 남습니다. Rust와 egui로 만든 macOS용 AI CLI 작업실, Runwick.",
    "a11y.skip": "본문으로 건너뛰기",
    "a11y.nav": "주 탐색",
    "nav.features": "기능",
    "nav.how": "사용 방법",
    "nav.install": "설치",
    "nav.download": "다운로드",
    "hero.eyebrow": "macOS를 위한 AI CLI 작업실",
    "hero.title": "CLI는 자유롭게.<br /><span>맥락은 그대로.</span>",
    "hero.sub": "Claude Code, Codex, 셸을 프로젝트별로 오가세요. 익숙한 CLI에서 하던 일을 이어 가고, 대화와 파일은 한곳에 남겨 두세요.",
    "hero.download": "macOS용 다운로드",
    "a11y.appInfo": "앱 정보",
    "hero.free": "무료 · MIT",
    "hero.live": "작업은 이어집니다",
    "a11y.workOpen": "작업 화면 크게 보기",
    "image.work": "Runwick에서 프로젝트 터미널과 README 문서를 나란히 연 화면",
    "hero.caption": "프로젝트 · 작업 공간 · 세션, 한 화면에서.",
    "principle": "쓰던 CLI 그대로. <strong>작업에 필요한 맥락을 곁에.</strong>",
    "features.kicker": "작업실 안에서",
    "features.title": "실행부터 다음 작업까지,<br />흐름이 끊기지 않도록.",
    "features.sub": "터미널, 기록, 문서를 프로젝트별로 연결합니다. 폴더 하나를 열면 기본 공간에서 바로 시작할 수 있습니다.",
    "f1.title": "실행 유지",
    "f1.body": "창을 닫아도 CLI는 계속 실행됩니다. 메뉴 막대에서 상태를 확인하고, 다시 열면 같은 실행에 연결됩니다.",
    "f2.title": "기록 검색과 핀",
    "f2.body": "지난 대화를 한국어 부분 검색으로 찾으세요. 다시 볼 메시지와 터미널 발췌는 핀으로 남길 수 있습니다.",
    "f3.title": "파일과 문서",
    "f3.body": "파일 트리에서 찾고, Markdown과 텍스트를 편집하고 미리 보세요. CLI와 편집이 겹치면 3열 비교로 확인합니다.",
    "f4.title": "위키와 타임라인",
    "f4.body": "문서의 연결은 위키 그래프로, 작업의 흐름은 날짜별 타임라인으로. AI 정리는 프로젝트별로 켜고, 배터리에서는 보류합니다.",
    "f5.title": "CLI끼리 일 맡기기",
    "f5.body": "<code>runwick delegate</code>로 다른 CLI에 일을 맡기고 결과를 받으세요. 필요하면 작업자를 살펴보거나 직접 대화할 수 있습니다.",
    "f6.title": "공통 환경변수와 세션 새로고침",
    "f6.body": "환경변수는 공통 설정으로 관리하고, 비밀값은 macOS 키체인에 둡니다. 설정이 바뀌어도 같은 대화를 유지하며 새로고침합니다.",
    "f7.title": "GitHub 백업·동기화",
    "f7.body": "기록, 핀, 위키, 설정을 비공개 저장소에 백업하고 다른 컴퓨터에서 이어 쓰세요. 설정의 비밀값은 동기화에서 제외합니다.",
    "f8.title": "여러 기기",
    "f8.body": "SSH로 다른 Mac의 Runwick에 연결하세요. 같은 사이드바에서 원격 작업 공간을 열고, 각 기기가 자신의 실행을 유지합니다.",
    "showcase.kicker": "쌓이는 맥락",
    "showcase.title": "끝난 대화도, 다음 작업의 시작.",
    "showcase.sub": "무엇을 결정했고 어디까지 했는지, 기록으로 돌아와 이어 가세요.",
    "showcase.graph.title": "흩어진 지식을 연결하고",
    "showcase.graph.body": "위키 문서와 근거 기록을 그래프로 살펴보고, AI가 제안한 변경은 검토한 뒤 반영합니다.",
    "a11y.graphOpen": "위키 그래프 크게 보기",
    "image.graph": "위키 문서의 링크와 근거 기록을 연결해 보여 주는 Runwick 관계 그래프",
    "showcase.timeline.title": "어제의 흐름을 이어 가세요",
    "showcase.timeline.body": "세션, 핀, 커밋, 위키와 협업 기록을 날짜별로 모아 봅니다. AI 요약이 없어도 실제 기록은 남습니다.",
    "a11y.timelineOpen": "타임라인 크게 보기",
    "image.timeline": "프로젝트별 주간 기록과 날짜별 작업 카드를 보여 주는 Runwick 타임라인",
    "how.kicker": "사용 방법",
    "how.title": "폴더를 열면, 작업실이 됩니다.",
    "how.s1.title": "폴더 열기",
    "how.s1.body": "프로젝트 폴더를 열면 기본 작업 공간이 준비됩니다. Git 저장소가 아닌 폴더도 사용할 수 있습니다.",
    "how.s2.title": "CLI 고르기",
    "how.s2.body": "설치한 Claude Code, Codex, 셸 중에서 고르세요. 익숙한 터미널에서 시작하고, 필요할 때 작업을 나누세요.",
    "how.s3.title": "창을 닫아도 계속",
    "how.s3.body": "진행 중인 작업은 그대로 두고 창을 닫으세요. 다시 열면 같은 실행에 연결되어 하던 일을 이어 갑니다.",
    "how.note": "재부팅 후에는 기록과 마지막 화면을 복원합니다. 다시 실행할 세션은 직접 선택합니다.",
    "install.kicker": "설치",
    "install.title": "Mac에 작은 작업실 하나.",
    "install.sub": "macOS 11 이상. 사용할 AI CLI는 별도로 설치하고 로그인해 주세요.",
    "install.s1.title": "다운로드",
    "install.s1.body": "<a href=\"https://github.com/gjeon03/runwick/releases/latest\">최신 릴리스</a>에서 Mac에 맞는 배포 파일을 받으세요.",
    "install.s2.title": "응용 프로그램으로 옮기기",
    "install.s2.body": "다운로드한 파일을 열고 <strong>Runwick.app</strong>을 <strong>응용 프로그램</strong> 폴더로 옮기세요.",
    "install.s3.title": "처음 열 때",
    "install.s3.body": "Finder에서 Runwick을 우클릭한 뒤 <strong>열기</strong>를 선택하세요. macOS가 계속 차단한다면 아래 안내를 확인하세요.",
    "install.help.title": "macOS가 실행을 막는다면",
    "install.help.body": "현재 번들은 ad-hoc 서명이며 Apple 공증은 아직 적용되지 않았습니다. 공식 GitHub 릴리스에서 받은 앱인지 확인한 뒤, 터미널에서 이 앱의 격리 속성만 제거할 수 있습니다.",
    "a11y.quarantine": "실행 차단 해제 명령",
    "install.help.after": "명령을 실행한 다음 응용 프로그램 폴더에서 Runwick을 다시 여세요.",
    "a11y.download": "Runwick 다운로드",
    "install.all": "모든 릴리스 보기",
    "install.free": "무료 · 오픈소스 · MIT 라이선스",
    "install.build.title": "소스에서 빌드",
    "install.build.body": "Rust stable과 Xcode 또는 Command Line Tools가 필요합니다. 저장소를 받은 뒤 아래 명령을 실행하세요.",
    "a11y.build": "소스 빌드 명령",
    "install.build.link": "빌드 안내",
    "faq.title": "궁금한 점이 있나요?",
    "faq.issues": "GitHub에 질문 남기기",
    "faq.q1": "Electron 앱인가요?",
    "faq.a1": "아니요. Rust와 egui로 만든 macOS용 네이티브 앱입니다. CLI는 앱 안의 터미널에서 원래 화면 그대로 실행됩니다.",
    "faq.q2": "비밀값이 GitHub에 올라가나요?",
    "faq.a2": "아니요. 비밀 환경변수 값은 macOS 키체인에 두고 백업·동기화에서 제외합니다. 기록과 위키 등은 연결한 비공개 저장소로 동기화하므로, 대화나 문서에 직접 적은 민감한 내용은 따로 확인해 주세요.",
    "faq.q3": "창을 닫으면 작업도 끝나나요?",
    "faq.a3": "창만 닫으면 CLI는 계속 실행되고 메뉴 막대에서 상태를 볼 수 있습니다. 작업을 멈추려면 ‘작업까지 종료’를 선택하세요. 컴퓨터를 재부팅한 뒤에는 기록과 화면을 복원하고, 재개할 세션을 직접 고릅니다.",
    "faq.q4": "어떤 CLI를 쓸 수 있나요?",
    "faq.a4": "Claude Code, Codex, 일반 셸을 지원합니다. 각 AI CLI의 설치와 로그인은 별도로 필요하며, 해당 서비스의 이용 조건과 요금이 적용됩니다.",
    "faq.q5": "다른 컴퓨터에서도 이어 쓸 수 있나요?",
    "faq.a5": "GitHub 백업·동기화로 기록을 다른 컴퓨터에서 이어 쓸 수 있습니다. SSH로 다른 Mac의 Runwick에 연결하면 그 기기의 작업 공간도 열 수 있습니다. 원격 실행은 구현되어 있지만 실제 두 Mac 사이의 연결 검증은 아직 남아 있습니다.",
    "faq.q6": "무료인가요? 라이선스는요?",
    "faq.a6": "Runwick은 무료 오픈소스이며 <a href=\"https://github.com/gjeon03/runwick/blob/main/LICENSE\">MIT 라이선스</a>로 공개합니다. 사용하는 AI CLI 서비스의 비용은 별도입니다.",
    "footer.tagline": "작업은 이어지고, 맥락은 남습니다."
  },
  "en": {
    "meta.title": "Runwick — a workroom for your AI CLIs",
    "meta.description": "Claude Code, Codex, and your shell, organized by project. Keep runs alive and context close. A native macOS workroom built with Rust and egui.",
    "a11y.skip": "Skip to content",
    "a11y.nav": "Main navigation",
    "nav.features": "Features",
    "nav.how": "How it works",
    "nav.install": "Install",
    "nav.download": "Download",
    "hero.eyebrow": "An AI CLI workroom for macOS",
    "hero.title": "Your CLIs.<br /><span>All in context.</span>",
    "hero.sub": "Move between Claude Code, Codex, and your shell, project by project. Work in the CLI you know, with your conversations and files close at hand.",
    "hero.download": "Download for macOS",
    "a11y.appInfo": "App information",
    "hero.free": "Free · MIT",
    "hero.live": "Keep the work running",
    "a11y.workOpen": "View the workroom screenshot at full size",
    "image.work": "Runwick with a project terminal and a README document open side by side",
    "hero.caption": "Projects, workspaces, and sessions. One workroom.",
    "principle": "The CLI you know. <strong>The context you need, right beside it.</strong>",
    "features.kicker": "Inside the workroom",
    "features.title": "From the first command<br />to what comes next.",
    "features.sub": "Terminals, conversations, and documents, connected by project. Open a folder and start in its default workspace.",
    "f1.title": "Keep runs alive",
    "f1.body": "Close the window without stopping your CLIs. Check their status in the menu bar, then reopen the app to reconnect to the same runs.",
    "f2.title": "Search and pin",
    "f2.body": "Find past conversations with keyword search, including partial Korean text. Pin messages and terminal excerpts you want to come back to.",
    "f3.title": "Files and documents",
    "f3.body": "Browse files, edit and preview Markdown and text. If a CLI changes a file you’re editing, review a three-way comparison.",
    "f4.title": "Wiki and timeline",
    "f4.body": "Explore document links in a wiki graph and work history in a daily timeline. AI organization is opt-in per project and deferred on battery.",
    "f5.title": "Let CLIs work together",
    "f5.body": "Use <code>runwick delegate</code> to hand work to another CLI and receive its results. Peek at a worker or take over the conversation when needed.",
    "f6.title": "Shared settings, same conversation",
    "f6.body": "Manage global environment variables, with secret values in macOS Keychain. Refresh a session to apply changes while keeping the same conversation.",
    "f7.title": "GitHub backup and sync",
    "f7.body": "Back up records, pins, wiki pages, and settings to a private repository, then continue on another computer. Secret setting values are excluded from sync.",
    "f8.title": "More than one Mac",
    "f8.body": "Connect to Runwick on another Mac over SSH. Open its workspaces from the same sidebar while each machine keeps its own runs alive.",
    "showcase.kicker": "Context that stays",
    "showcase.title": "Past conversations. Future starting points.",
    "showcase.sub": "Return to the decisions you made and the work you left off, then pick up from there.",
    "showcase.graph.title": "Connect what you’ve learned",
    "showcase.graph.body": "Explore wiki pages and their supporting records in a graph. Review AI-proposed changes before applying them.",
    "a11y.graphOpen": "View the wiki graph screenshot at full size",
    "image.graph": "Runwick wiki graph connecting documents, links, and supporting conversation records",
    "showcase.timeline.title": "Pick up yesterday’s thread",
    "showcase.timeline.body": "Browse sessions, pins, commits, wiki changes, and collaboration by date. The actual records remain available without AI summaries.",
    "a11y.timelineOpen": "View the timeline screenshot at full size",
    "image.timeline": "Runwick timeline showing weekly project activity and dated work cards",
    "how.kicker": "How it works",
    "how.title": "Open a folder. Make it a workroom.",
    "how.s1.title": "Open a folder",
    "how.s1.body": "Your project opens in a ready-to-use default workspace. Folders don’t need to be Git repositories.",
    "how.s2.title": "Choose a CLI",
    "how.s2.body": "Pick your installed Claude Code, Codex, or shell. Start in the terminal you know and delegate work when you need to.",
    "how.s3.title": "Close the window. Keep going.",
    "how.s3.body": "Leave your work running when you close the window. Reopen it to reconnect to the same runs and carry on.",
    "how.note": "After a reboot, Runwick restores your records and layout. You choose which supported sessions to resume.",
    "install.kicker": "Install",
    "install.title": "A place for your work, on your Mac.",
    "install.sub": "macOS 11 or later. Install and sign in to the AI CLIs you want to use separately.",
    "install.s1.title": "Download",
    "install.s1.body": "Get the package for your Mac from the <a href=\"https://github.com/gjeon03/runwick/releases/latest\">latest release</a>.",
    "install.s2.title": "Move to Applications",
    "install.s2.body": "Open the downloaded file and move <strong>Runwick.app</strong> into your <strong>Applications</strong> folder.",
    "install.s3.title": "Open for the first time",
    "install.s3.body": "Right-click Runwick in Finder and choose <strong>Open</strong>. If macOS still blocks it, follow the instructions below.",
    "install.help.title": "If macOS blocks the app",
    "install.help.body": "The current bundle is signed ad hoc and is not yet notarized by Apple. After confirming you downloaded it from the official GitHub release, you can remove the quarantine attribute for this app in Terminal.",
    "a11y.quarantine": "Command to remove the app’s quarantine attribute",
    "install.help.after": "Then open Runwick again from your Applications folder.",
    "a11y.download": "Download Runwick",
    "install.all": "View all releases",
    "install.free": "Free · Open source · MIT license",
    "install.build.title": "Build from source",
    "install.build.body": "Requires stable Rust and Xcode or Command Line Tools. Clone the repository, then run this command.",
    "a11y.build": "Build command",
    "install.build.link": "Build instructions",
    "faq.title": "A few things to know.",
    "faq.issues": "Ask on GitHub",
    "faq.q1": "Is this an Electron app?",
    "faq.a1": "No. Runwick is a native macOS app built with Rust and egui. Your CLIs run with their original interfaces in its terminal.",
    "faq.q2": "Are secrets uploaded to GitHub?",
    "faq.a2": "No. Secret environment values stay in macOS Keychain and are excluded from backup and sync. Records and wiki pages sync to your connected private repository, so check any sensitive information you typed directly into conversations or documents.",
    "faq.q3": "What happens when I close the window?",
    "faq.a3": "Your CLIs keep running, with status available in the menu bar. Choose “Stop work and quit” to stop them. After a reboot, Runwick restores your records and layout; you choose which sessions to resume.",
    "faq.q4": "Which CLIs can I use?",
    "faq.a4": "Claude Code, Codex, and a regular shell. AI CLIs need to be installed and signed in separately. Their service terms and fees still apply.",
    "faq.q5": "Can I continue on another computer?",
    "faq.a5": "GitHub backup and sync lets you carry your records to another computer. Connect over SSH to open workspaces on another Mac running Runwick. Remote execution is implemented, but validation between two real Macs is still pending.",
    "faq.q6": "Is it free? What’s the license?",
    "faq.a6": "Runwick is free and open source under the <a href=\"https://github.com/gjeon03/runwick/blob/main/LICENSE\">MIT license</a>. Any fees for your AI CLI services are separate.",
    "footer.tagline": "Keep the work going. Keep the context."
  }
};

const LANGUAGE_KEY = 'runwick-lang';

/** @param {string | null | undefined} value @returns {Language | null} */
function languageOrNull(value) {
  return value === 'ko' || value === 'en' ? value : null;
}

/** @returns {Language | null} */
function storedLanguage() {
  try {
    return languageOrNull(localStorage.getItem(LANGUAGE_KEY));
  } catch {
    return null;
  }
}

/** @param {Language} language @param {boolean} [persist] */
function setLanguage(language, persist = false) {
  const messages = MESSAGES[language];
  document.documentElement.lang = language;

  document.querySelectorAll('[data-i18n], [data-i18n-html]').forEach((node) => {
    const html = node.hasAttribute('data-i18n-html');
    const key = node.getAttribute(html ? 'data-i18n-html' : 'data-i18n');
    const value = key ? messages[key] : undefined;
    if (value === undefined) return;
    if (html) {
      node.innerHTML = value;
    } else {
      node.textContent = value;
    }
  });

  /** @type {Array<[string, string]>} */
  const translatedAttributes = [['data-i18n-label', 'aria-label'], ['data-i18n-alt', 'alt']];
  translatedAttributes.forEach(([marker, attribute]) => {
    document.querySelectorAll(`[${marker}]`).forEach((node) => {
      const key = node.getAttribute(marker);
      if (key && messages[key]) node.setAttribute(attribute, messages[key]);
    });
  });

  document.querySelectorAll('[data-lang-toggle]').forEach((button) => {
    button.textContent = language === 'ko' ? 'EN' : 'KO';
    button.setAttribute('aria-label', language === 'ko' ? 'Switch to English' : '한국어로 전환');
    button.setAttribute('lang', language === 'ko' ? 'en' : 'ko');
    button.removeAttribute('hidden');
  });
  document.title = messages['meta.title'];
  document.querySelector('meta[name="description"]')?.setAttribute('content', messages['meta.description']);
  document.querySelector('meta[property="og:title"]')?.setAttribute('content', messages['meta.title']);
  document.querySelector('meta[property="og:description"]')?.setAttribute('content', messages['meta.description']);
  document.querySelector('meta[property="og:image:alt"]')?.setAttribute('content', messages['image.work']);
  document.querySelector('meta[property="og:locale"]')?.setAttribute('content', language === 'ko' ? 'ko_KR' : 'en_US');

  if (persist) {
    try {
      localStorage.setItem(LANGUAGE_KEY, language);
    } catch {
      // Language switching still works when browser storage is unavailable.
    }
  }
}

const browserLanguage = navigator.language?.toLowerCase();
const preferredLanguage = storedLanguage()
  ?? languageOrNull(new URLSearchParams(location.search).get('lang'))
  ?? (browserLanguage ? (browserLanguage.startsWith('ko') ? 'ko' : 'en') : 'ko');

setLanguage(preferredLanguage);

document.querySelectorAll('[data-lang-toggle]').forEach((button) => {
  button.addEventListener('click', () => {
    setLanguage(document.documentElement.lang === 'ko' ? 'en' : 'ko', true);
  });
});
