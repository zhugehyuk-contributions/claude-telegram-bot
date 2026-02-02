# WSL Systemd User Service Guide

## Issue Report: 2026-02-02 무한 재시작 루프

### 증상
- 봇이 계속 재시작됨 ("✅ Bot restarted" 메시지 반복)
- Telegram API 409 Conflict 에러 발생
- 여러 봇 인스턴스가 동시 실행

### 원인
1. **DBUS 환경변수 누락**: `systemctl --user` 명령이 제대로 동작하지 않음
2. **서비스 파일 중복**: `claude-telegram-bot.service`와 `elon-bot.service` 둘 다 같은 디렉토리를 가리킴
3. **Restart=always**: 서비스가 죽어도 systemd가 계속 재시작
4. **서비스 마스킹 안 됨**: `disable`만으로는 이미 로드된 서비스 중지 불가

### 해결
```bash
# WSL에서 systemctl --user 사용시 DBUS 환경변수 필요
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus

# 서비스 중지 + 비활성화 + 마스킹
systemctl --user stop claude-telegram-bot
systemctl --user disable claude-telegram-bot
systemctl --user mask claude-telegram-bot  # /dev/null로 링크 → 절대 시작 안 됨
```

---

## WSL Systemd User Service 구조

### 서비스 파일 위치
```
~/.config/systemd/user/
├── chaewon-bot.service      # 채원봇
├── elon-bot.service         # 엘론봇 (마스킹됨)
├── claude-telegram-bot.service  # (마스킹됨)
└── user-sshd.service        # SSH 데몬
```

### 서비스 상태 확인
```bash
# 환경변수 설정 (필수!)
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus

# 모든 사용자 서비스 목록
systemctl --user list-units --type=service

# 특정 서비스 상태
systemctl --user status chaewon-bot

# 서비스 로그
journalctl --user -u chaewon-bot -f
```

### 서비스 이름 설정

`.env` 파일에 `SERVICE_NAME` 추가:
```bash
# .env
SERVICE_NAME=elon-bot
```

없으면 디렉토리 이름 사용 (예: `claude-telegram-bot.p9`)

### 서비스 파일 (자동 생성됨)
```ini
# ~/.config/systemd/user/elon-bot.service
[Unit]
Description=elon-bot
After=network.target

[Service]
Type=simple
WorkingDirectory=/home/zhugehyuk/2lab.ai/claude-telegram-bot.p9
ExecStart=/home/zhugehyuk/.bun/bin/bun run start
Restart=always
RestartSec=10
Environment=PATH=/home/zhugehyuk/.bun/bin:/usr/local/bin:/usr/bin:/bin
StandardOutput=append:/tmp/elon-bot.log
StandardError=append:/tmp/elon-bot.err

[Install]
WantedBy=default.target
```

---

## make up 워크플로우

### WSL 전체 흐름 (완전 재설치)
```
make up
  ├── install              # bun install
  ├── build                # bun run typecheck
  └── deploy
      ├── stop service     # 기존 서비스 중지
      ├── disable service  # 서비스 비활성화
      ├── unmask service   # 마스킹 해제
      ├── remove service   # 서비스 파일 삭제
      ├── create service   # 새 서비스 파일 생성
      ├── daemon-reload    # systemd 리로드
      ├── enable service   # 서비스 활성화
      └── start service    # 서비스 시작
```

### 플랫폼별 동작

| 플랫폼 | 동작 |
|--------|------|
| WSL | 서비스 완전 재설치 (stop → remove → install → start) |
| macOS | 서비스 재시작 (launchctl unload/load) |

### Make 타겟 목록

| 타겟 | 설명 |
|------|------|
| `make up` | 빌드 + 서비스 완전 재설치 |
| `make start` | 서비스 시작 |
| `make stop` | 서비스 중지 |
| `make restart` | 서비스 재시작 |
| `make status` | 서비스 상태 확인 |
| `make logs` | 로그 보기 |
| `make install-service` | 서비스 설치 (수동) |
| `make uninstall-service` | 서비스 제거 |
| `make reinstall-service` | 서비스 재설치 |

---

## Rust 포트 (ctb)

Rust 바이너리로 실행/배포하려면:

| 타겟 | 설명 |
|------|------|
| `make build-rust` | Rust release 빌드 (`ctb`, `ctb-ask-user-mcp`) |
| `make up-rust` | Rust 빌드 + 서비스 재시작/설치(플랫폼별) |
| `make start-rust` | Rust 시작 |
| `make stop-rust` | Rust 중지 |
| `make status-rust` | Rust 상태 |

기본 서비스명은 `$(SERVICE_NAME)-rs` (Makefile 변수 `RUST_SERVICE_NAME`)이며,
WSL에서는 `~/.config/systemd/user/$(RUST_SERVICE_NAME).service` 로 생성됩니다.

---

## 일반 배포 (권장)

```bash
# 한 줄로 끝
make up
```

`make up`은 자동으로:
1. 의존성 설치 (`bun install`)
2. 타입 체크 (`bun run typecheck`)
3. 기존 서비스 완전 제거
4. 새 서비스 설치
5. 서비스 시작

### 수동 서비스 관리 (필요시)

```bash
# DBUS 환경변수 설정 필수
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus

# 서비스 완전 제거
make uninstall-service

# 서비스 재설치
make reinstall-service

# 서비스만 시작/중지
make start
make stop
make restart
```

---

## 트러블슈팅

### "Failed to connect to bus: No medium found"
```bash
# 원인: DBUS 환경변수 없음
# 해결:
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus
```

### 서비스가 계속 재시작됨
```bash
# 1. 서비스 마스킹 (완전 차단)
systemctl --user mask elon-bot

# 2. 실행 중인 프로세스 확인
ps aux | grep "bun run" | grep index

# 3. 프로세스 트리로 부모 확인
pstree -ps <PID>

# 4. 어떤 서비스가 실행 중인지 확인
systemctl --user status
```

### 여러 프로젝트 복사본 사용시

각 복사본마다 `.env`에 고유한 `SERVICE_NAME` 설정:
```bash
# project1/.env
SERVICE_NAME=chaewon-bot

# project2/.env
SERVICE_NAME=elon-bot
```

이렇게 하면:
- 서비스 파일: `~/.config/systemd/user/{SERVICE_NAME}.service`
- PID 파일: `/tmp/{SERVICE_NAME}.pid`
- 로그 파일: `/tmp/{SERVICE_NAME}.log`

### 여러 봇 인스턴스 충돌 (409 Conflict)
```bash
# Telegram은 하나의 봇 토큰당 하나의 polling만 허용
# 여러 인스턴스 → 409 에러

# 모든 봇 프로세스 확인
ps aux | grep "bun run" | grep index

# 디렉토리별 구분
for pid in $(pgrep -f "bun run src/index.ts"); do
  echo "PID $pid: $(ls -l /proc/$pid/cwd 2>/dev/null | awk '{print $NF}')"
done

# 특정 디렉토리 프로세스만 종료
pkill -f "telegram-bot.p9"
```

### 서비스 로그 확인
```bash
# systemd 저널
journalctl --user -u elon-bot -f

# 또는 파일 로그
tail -f /tmp/claude-telegram-bot.log
tail -f /tmp/claude-telegram-bot.err
```

---

## 참고: .bashrc 설정

WSL에서 항상 DBUS 환경변수 사용하려면:
```bash
# ~/.bashrc에 추가
if [ -S /run/user/$(id -u)/bus ]; then
  export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus
fi
```

---

*작성일: 2026-02-02 | 작성자: Claude Code*
