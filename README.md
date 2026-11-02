# TS Talk Overlay

[English](#english) | [Русский](#русский)

## English

A TeamSpeak 3 plugin that shows on top of your game who is talking in TeamSpeak right now.

- **"Кто говорит" (Talking)**: who is talking right now. Whispers on top (orange dot), channel voice below (green dot). Someone who has just stopped stays visible, dimmed, for a few seconds.
- **"Список канала" (Channel list)**: everyone in your channel. The dot turns green while that person talks. Someone who has just joined is highlighted in blue.
- **Per-person volume** can be changed right from the Talking window with the mouse wheel.

The windows do not get in the way of the game: clicks pass through them and they never take focus. The game must run in borderless windowed mode; in exclusive fullscreen the windows end up under the game.

If TeamSpeak runs on another computer in the same network, see [TS LAN Overlay](https://github.com/amveng/ts3_lan_overlay).

The user interface is in Russian.

### Installation

1. Download `ts3_talk_overlay_setup_X.Y.Z.exe` from [Releases](https://github.com/amveng/ts3_talk_overlay/releases).
2. Close TeamSpeak 3 and run the installer. It finds the plugins folder by itself.
3. In TeamSpeak open Tools → Options → Addons and enable "TS Talk Overlay".

### Controls

| Key | Action |
|---|---|
| Ctrl+Alt+T | hide or show the Talking window |
| Ctrl+Alt+L | hide or show the Channel list |
| Ctrl+Alt+V | volume mode |
| Ctrl+Alt+M | arrange mode |

**Volume mode:** the Talking window gets a green frame and starts taking the mouse. The wheel over a nickname makes that person louder or quieter in 1 dB steps; a right click resets to 0. Pressing Ctrl+Alt+V again or 10 seconds without action returns to normal. While the mouse is over the window the list is frozen, so the wheel never lands on someone else when a new speaker appears. The plugin remembers the volume by the person's unique ID (`ts3_talk_overlay_volumes.txt` next to the DLL) and applies it again when they rejoin and after a TeamSpeak restart; a volume changed later in TeamSpeak itself takes priority.

**Arrange mode:** both windows are shown, even when empty (with sample rows), get a blue frame and take the mouse. Drag the middle to move a window, the left or right edge to change its width, the top or bottom edge or a corner to change the font size. When you release the mouse the monitor, corner, offsets, width and font size are saved to the settings. Ctrl+Alt+M again returns to normal.

Everything else is set in TeamSpeak: Options → Addons → TS Talk Overlay → Settings (monitor, corner, offsets, sizes, font, colours, opacity, timings, hotkeys).

### Building from source

Requires [Rust](https://rustup.rs) (MSVC) and [Inno Setup 6](https://jrsoftware.org/isinfo.php).

```powershell
cargo build --release            # target\release\ts3_talk_overlay.dll
.\installer\build.ps1            # installer in dist\
```

The plugin is written in Rust without SDK wrappers: TeamSpeak functions are called by their index in `struct TS3Functions` (API 26, client 3.6).

### Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

Installers are built from the source code in this repository by [GitHub Actions](.github/workflows/build.yml), and every release is signed only after manual approval.

Team roles:
- Committers and reviewers: [TakeChance](https://github.com/amveng)
- Approvers: [TakeChance](https://github.com/amveng)

### Privacy policy

This program will not transfer any information to other networked systems unless specifically requested by the user or the person installing or operating it.

The plugin only reads data from TeamSpeak on this computer and draws the windows. Settings, volumes and the log are kept in the TeamSpeak plugins folder.

---

## Русский

Плагин для TeamSpeak 3, который показывает поверх игры, кто сейчас говорит в TeamSpeak.

- **«Кто говорит»**: кто говорит прямо сейчас. Шёпот наверху (оранжевая точка), ниже голос в канале (зелёная точка). Замолчавший ещё несколько секунд виден приглушённым.
- **«Список канала»**: все, кто в вашем канале. У говорящего точка загорается зелёным. Ник того, кто только что зашёл, подсвечивается голубым.
- **Громкость отдельного человека** меняется прямо из окна «Кто говорит», колесом мыши.

Окна не мешают игре: клики проходят сквозь них, фокус они не забирают. Игра должна быть в режиме «Оконный без рамки» (Borderless). В полноэкранном режиме окна окажутся под игрой.

Если TeamSpeak запущен на другом компьютере в той же сети, смотрите [TS LAN Overlay](https://github.com/amveng/ts3_lan_overlay).

### Установка

1. Скачайте `ts3_talk_overlay_setup_X.Y.Z.exe` со страницы [Releases](https://github.com/amveng/ts3_talk_overlay/releases).
2. Закройте TeamSpeak 3 и запустите установщик. Папку плагинов он найдёт сам.
3. В TeamSpeak откройте Инструменты → Параметры → Дополнения и включите «TS Talk Overlay».

Установщик не подписан, поэтому Windows может показать «Система Windows защитила ваш компьютер». Нажмите «Подробнее» → «Выполнить в любом случае».

Подробная инструкция лежит в [Инструкция по установке.txt](Инструкция%20по%20установке.txt), она же копируется рядом с плагином.

### Управление

| Клавиша | Что делает |
|---|---|
| Ctrl+Alt+T | скрыть или показать «Кто говорит» |
| Ctrl+Alt+L | скрыть или показать «Список канала» |
| Ctrl+Alt+V | режим громкости |
| Ctrl+Alt+M | режим расположения окон |

**Режим громкости:** окно «Кто говорит» получает зелёную рамку и начинает ловить мышь. Колесо над ником делает человека громче или тише с шагом 1 дБ, правый щелчок сбрасывает на 0. Повторное Ctrl+Alt+V или 10 секунд без действий возвращают обычный режим. Пока мышь над окном, список заморожен: порядок и число строк не меняются, обновляются только точки и громкость; новые говорящие появляются, когда мышь уходит. Плагин запоминает громкость по уникальному ID человека (`ts3_talk_overlay_volumes.txt` рядом с DLL) и выставляет её снова при перезаходе и после перезапуска TeamSpeak; громкость, изменённая потом в самом TeamSpeak, имеет приоритет.

**Режим расположения:** оба окна видны, даже если пусты (с образцами строк), получают голубую рамку и ловят мышь. За середину окно перетаскивается, левый и правый край меняют ширину, верхний и нижний край и углы меняют размер шрифта (высота подстраивается под строки). Когда отпускаете мышь, монитор, угол, отступы, ширина и шрифт записываются в настройки; угол выбирается по четверти монитора, в которой оказалось окно. Повторное Ctrl+Alt+M возвращает обычный режим.

Остальное меняется в TeamSpeak: Параметры → Дополнения → TS Talk Overlay → «Настройки». Там монитор, угол экрана, отступы, размеры, шрифт, цвета, прозрачность, время показа и горячие клавиши.

### Сборка из исходников

Нужны [Rust](https://rustup.rs) (MSVC) и [Inno Setup 6](https://jrsoftware.org/isinfo.php).

```powershell
cargo build --release            # target\release\ts3_talk_overlay.dll
.\installer\build.ps1            # установщик в dist\
```

Плагин написан на Rust без SDK-обёрток: функции TeamSpeak вызываются по индексам `struct TS3Functions` (API 26, клиент 3.6).

### Подпись кода

Бесплатная подпись кода — [SignPath.io](https://about.signpath.io), сертификат — [SignPath Foundation](https://signpath.org).

Установщики собираются из исходников этого репозитория в [GitHub Actions](.github/workflows/build.yml), и каждый релиз подписывается только после ручного подтверждения.

Роли:
- авторы и ревьюеры: [TakeChance](https://github.com/amveng);
- подтверждают подпись: [TakeChance](https://github.com/amveng).

### Приватность

Плагин ничего не передаёт по сети. Он только читает данные из TeamSpeak на этом компьютере и рисует окна; настройки, громкости и лог хранятся в папке плагинов TeamSpeak.

---

## License / Лицензия

[MIT](LICENSE)
