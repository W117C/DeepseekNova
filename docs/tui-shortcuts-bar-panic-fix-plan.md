# План: паника shortcuts-строки TUI на не-ASCII языках (`ru`/`zh`)

- **Дата:** 2026-10-11
- **Статус:** план, реализация не начата
- **Приоритет:** P0 — при `DEEPSEEKNOVA_LANG=ru` (и `zh`) TUI падает на первом кадре, полностью неработоспособен
- **Область:** `crates/deepseeknova-tui` (single crate; протокол §1.1 AGENTS.md не требуется, но изменение поведения UI — с явным согласованием)

---

## 1. Симптом и воспроизведение

```
$ RUST_BACKTRACE=1 ./deepseeknova-cli chat --tui
thread 'main' panicked at crates/deepseeknova-tui/src/render/message.rs:1348:54:
end byte index 3 is not a char boundary; it is inside 'к' (bytes 2..4 of string)
```

Стек: `AppState::draw` → `render/message.rs:1348` → `str` index panic.

**Минимальное воспроизведение:**

```bash
DEEPSEEKNOVA_LANG=ru cargo run -p deepseeknova-cli -- chat --tui   # паника на первом кадре
DEEPSEEKNOVA_LANG=zh cargo run -p deepseeknova-cli -- chat --tui   # паника на первом кадре
cargo run -p deepseeknova-cli -- chat --tui                        # en: работает
```

Терминал не нужен: паника воспроизводится на `TestBackend` (см. §7, тест 1).

---

## 2. Точка отказа (root cause)

`crates/deepseeknova-tui/src/render/message.rs:1341-1360` (shortcuts-строка, стиль grok `ShortcutsBar`):

```rust
let hint_text = crate::render::status::hint_for(self.focus, self.tr);
let mut hint_spans: Vec<Span<'static>> = Vec::new();
for (i, seg) in hint_text.split(" · ").enumerate() {
    if i > 0 {
        hint_spans.push(Span::styled("  │  ", Style::default().fg(theme.gray_dim)));
    }
    // 段内首个 `Ctrl+`/`Shift+`/`Esc`/`Tab` 键位前缀加粗。
    let styled_seg = if seg.len() >= 3 && seg[..3].contains('+') {   // ← строка 1348: байтовый срез
        ...
```

`seg[..3]` — срез **по байтам**, а не по символам. Паникует на любом сегменте, у которого 3-й байт попадает внутрь многобайтового символа.

**Прямое совпадение с трейсом.** Шаблон `HintInput` для `ru` (`i18n/keys.rs:1580`):

```
/ команды · Ctrl+P режим прав · Ctrl+T мышь · ...
```

Первый сегмент `/ команды`: `/` = байт 0, ` ` = байт 1, `к` = байты 2..4. `seg[..3]` пытается разрезать `к` → ровно сообщение из паники. Для `zh` (`/ 命令`, `命` = байты 2..5) — тот же дефект.

**Почему en не падает:** `/ commands` — все байты ASCII, граница на индексе 3 существует.

**Область поражения шире, чем один шаблон.** Любой не-ASCII hint-шаблон даёт панику почти в каждом сегменте вида `<1 ASCII-символ><пробел><кириллица/хан>`: например `ru` `HintConversation` = `{nav} навигация · ...` → сегмент `j навигация` (`н` = байты 2..4). То есть падает не только `Focus::Input`.

---

## 3. Почему дефект не поймали раньше

1. `i18n/keys.rs` покрыт тестами, но они проверяют **содержимое** строки (`status.rs:626 hint_text_per_focus`), а не рендер.
2. Существующие `TestBackend`-тесты (`render/*.rs`) вызывают `render_blocks` / отдельные виджеты, но **не** `AppState::draw`. Единственный вызов `app.draw` — `app/mod.rs:205` в рабочем цикле (проверено `grep -rn '\.draw('`), т.е. тестами не покрыт.
3. `cargo test` на CI запускается без `DEEPSEEKNOVA_LANG` → язык `En` по умолчанию, ветка `Ru`/`Zh` в рендере не исполняется.

Вывод: нужен тест, который исполняет **именно** `draw`/`shortcuts_spans` для всех языков.

---

## 4. Сопутствующий дефект: ветка «жирный ключ» недостижима

Проверка текущего условия по всем реальным шаблонам (`en`/`zh`/`ru`, все `Focus`):

- `Ctrl+P` → `seg[..3]` = `"Ctr"` → `'+'` нет → не жирный;
- `Shift+Enter` → `"Shi"` → нет;
- `Esc`, `Tab`, `Enter`, `y`, `↑↓`, `j/k` → нет.

То есть эвристика «в первых 3 **байтах** есть `+`» не срабатывает ни на одной реальной подсказке: заявленное в комментарии выделение клавиш-префиксов фактически не работает, все сегменты рендерятся `theme.gray`. Это самостоятельный (не фатальный) дефект; решение — см. §5 вариант B.

---

## 5. Решение

Логику сборки shortcuts-строки вынести в **чистую функцию** (тестируемую без терминала и без `Frame`):

```rust
/// shortcuts-строка: `hint_for` 文案按 ` · ` 分段，段间 `  │  ` 分隔。
/// 段内键位前缀（`Ctrl+P` / `Shift+Enter` / …）以 text_secondary 加粗，
/// 动作文案用 gray。纯函数：不依赖 Frame，便于按语言回归测试。
fn shortcuts_spans(hint_text: &str, theme: &Theme) -> Vec<Span<'static>> { … }
```

### Вариант A — минимальный (обязательный, закрывает P0)

Сохранить текущую семантику, но сделать её **символьной**:

```rust
/// 段首键位前缀判定：首 3 个字符含 `+`（`X+Y` 形态）。
/// 注意按字符而非字节取前缀——多字节文案（ru/zh）下字节切片会落在字符内部。
fn has_key_prefix(seg: &str) -> bool {
    seg.chars().take(3).any(|c| c == '+')
}
```

- Поведение для ASCII-текста **побитово идентично** прежнему (`seg[..3]` для ASCII = первые 3 символа).
- Паника устранена для всех языков и всех фокусов.
- Ветка `BOLD` остаётся недостижимой (как и до правки) — регрессии нет, но и пользы нет.

### Вариант B — рекомендуемый (P1, восстановление заявленного поведения)

Выделять **ведущий токен-ключ** сегмента, а не сегмент целиком:

- ключ = первый токен (до пробела), если он содержит `+` (`Ctrl+P`, `Shift+Enter`, `Ctrl+X Ctrl+E`) **или** входит в фиксированный набор имён клавиш (`Esc`, `Tab`, `Enter`, `Backspace`, `PageUp`, `PageDown`, `Home`, `End`, `F1`, `↑`, `↓`, `↑↓`, `y`, `n`);
- ключ → `theme.text_secondary` + `Modifier::BOLD`, остаток сегмента → `theme.gray`;
- сегмент без распознанного ключа → целиком `gray` (как сейчас).

Результат для `ru`: `Ctrl+P` жирным, `режим прав` — серым. Совпадает с комментарием в коде и с en-подсказками.

**Решение владельца:** A сразу, B — в этом же MR или отдельным (см. §13).

---

## 6. Правка: файлы и строки

| Файл | Что делаем |
|---|---|
| `crates/deepseeknova-tui/src/render/message.rs` | строки 1341-1361: заменить инлайновый цикл на вызов `shortcuts_spans(&hint_text, &theme)`; добавить `shortcuts_spans` + (для B) `key_prefix_len` рядом с `draw`, с `///`-доками (конвенция §4 AGENTS.md) |
| `crates/deepseeknova-tui/src/render/message.rs` | блок `#[cfg(test)] mod tests` (строка 1430): добавить тесты §7 |
| `AGENTS.md` | §5: запись о классе ошибки (см. §11) |
| `CHANGELOG.md` | `## [Unreleased]` → `### Fixed`: паника shortcuts-строки на ru/zh |

Итоговый вид целевого блока (вариант A):

```rust
        // ── shortcuts 栏（grok 对齐 ShortcutsBar）──────────
        let hint_text = crate::render::status::hint_for(self.focus, self.tr);
        let hint = Paragraph::new(Line::from(shortcuts_spans(&hint_text, &theme)));
        f.render_widget(hint, hint_area);
```

Публичный API crate не меняется (`shortcuts_spans` — приватная функция модуля), зависимости не добавляются.

---

## 7. Тесты (порядок TDD: красный → зелёный)

**Тест 1 — воспроизведение (сначала красный, до правки).**
Полный кадр `AppState::draw` на `TestBackend` для `ru`/`zh`/`en`:

```rust
#[test]
fn draw_does_not_panic_for_non_ascii_hints() {
    // 回归：shortcuts 段前缀曾是字节切片 seg[..3]，ru/zh 文案在字符内部
    // 切分 → 'end byte index 3 is not a char boundary'（TUI 首帧 panic）。
    for lang in [Lang::En, Lang::Zh, Lang::Ru] {
        let mut app = AppState { tr: Tr::new(lang), ..Default::default() };
        let backend = ratatui::backend::TestBackend::new(120, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();   // 修前：ru/zh паникует здесь
    }
}
```

Зафиксировать вывод паники в описании MR (доказательство «красного»).

**Тест 2 — чистая функция, все языки × все фокусы** (закрывает класс ошибки целиком):

```rust
#[test]
fn shortcuts_spans_never_panic_and_keep_text() {
    let theme = Theme::default();
    for lang in Lang::ALL {
        for focus in [Focus::Input, Focus::Conversation, Focus::Sidebar,
                      Focus::Completion, Focus::Help, Focus::Confirm] {
            let text = crate::render::status::hint_for(focus, Tr::new(lang));
            let spans = shortcuts_spans(&text, &theme);
            let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
            let expected = text.split(" · ").collect::<Vec<_>>().join("  │  ");
            assert_eq!(joined, expected, "{lang:?}/{focus:?}: 文本与分隔符不得丢失");
        }
    }
}
```

**Тест 3 — стили (вариант A: сегменты без `+` — gray; вариант B: ключ жирный, хвост серый):**

```rust
#[test]
fn shortcuts_spans_bold_key_prefix() { /* ru: "/ команды · Ctrl+P режим прав" */ }
```

Для A ассерты: сегмент `Ctrl+P режим прав` — `fg == theme.gray` (текущее фактическое поведение, фиксируем как «не BOLD»); разделитель `"  │  "` — `theme.gray_dim`.
Для B: span'ы `Ctrl+P` (BOLD, `text_secondary`) и ` режим прав` (`gray`).

**Тест 4 (опционально, для B) — распознавание ключей:** табличный тест `key_prefix_len` на `Ctrl+P`, `Shift+Enter`, `Esc`, `Tab`, `↑↓`, `y`, `/`, `режим` (негативные случаи).

---

## 8. Аудит остальных байтовых срезов в `deepseeknova-tui`

Полный grep по `[..N]` / `[a..b]` в crate — проверено вручную, **кроме строки 1348 небезопасных нет**:

| Место | Вердикт |
|---|---|
| `render/message.rs:581` | `chars[..N]` по `Vec<char>` — безопасно |
| `render/message.rs:1021` | `help.lines[start..end]` по `Vec` — безопасно |
| `render/status.rs:551,553` | индексы из `find('[')` / `rfind(']')` — границы по построению |
| `render/status.rs:673` | `bar_cells[..3]` по `Vec` — безопасно |
| `render/sidebar.rs:255` | `date[4..6]`/`[6..8]` после `is_ascii_digit` по 8 символам — безопасно |
| `input/editor.rs` (все срезы) | по `self.cursor` (всегда на границе) и по `char_indices` / `rfind('\n')` — безопасно |
| `input/at_complete.rs:46,61,63` | `text[..cursor]`, `@`+1 (ASCII) — безопасно |
| `input/md_highlight.rs:138,146` | ASCII-префиксы `"- "` / `"N. "` — безопасно |
| `model/apply.rs:322` | уже использует `is_char_boundary`-цикл — безопасно |
| `commands/builtin.rs:81` | срез `Vec<&str>` — безопасно |

Смежные crate вне области (для справки, не трогаем): `agent/loop_impl.rs` использует `floor_char_boundary`, `cli/chat.rs::truncate` — тоже; там риск не подтверждён.

---

## 9. Верификация

```bash
# 1. Красный тест (до правки) — зафиксировать панику
cargo test -p deepseeknova-tui draw_does_not_panic_for_non_ascii_hints

# 2. После правки — зелёный + весь crate
cargo test -p deepseeknova-tui

# 3. Формат и полный CI-эквивалент (fmt + clippy -D warnings + test + doc)
cargo fmt --all
make check

# 4. Ручной smoke (интерактивно, вручную; Esc Esc — выход)
DEEPSEEKNOVA_LANG=ru cargo run -p deepseeknova-cli -- chat --tui
DEEPSEEKNOVA_LANG=zh cargo run -p deepseeknova-cli -- chat --tui
```

Число тестов в README (`2125`, badge + таблица) **локально не трогаем**: `scripts/sync-test-count.py` синхронизирует по Linux CI и на macOS отказывается перезаписывать (`make test-count-check` на не-Linux пропускает сравнение). Обновление придёт с CI.

---

## 10. Риски и откат

| Риск | Оценка |
|---|---|
| Вариант A меняет поведение для не-ASCII текста | Нет: прежнее поведение было паникой; для ASCII — идентично |
| Вариант B меняет вид shortcuts-строки (ключи становятся жирными) | Да, осознанное UI-изменение — требует согласования (§13); затрагивает только оформление одной строки |
| Тест 1 на `TestBackend` может зависеть от размеров терминала | Минимизировано: фиксированный `120x24`, `AppState::default()` (welcome=false → основной путь рендера) |
| Поломка существующих тестов строки shortcuts | Отсутствуют (тестов на `hint_spans` нет) |

**Откат:** правка локальна — revert одного коммита; миграций и изменений данных нет.

---

## 11. Документация

- **`AGENTS.md` §5** (запись о классе ошибки, формат `[ошибка] / [как избежать]`):

  ```
  - [字符串按字节切片在多语言文案下 panic]：shortcuts 段前缀判定用 `seg[..3]`
    （字节）判断键位，ru/zh 文案（`/ команды`）第 3 字节落在 `к` 内部，
    TUI 首帧 panic；i18n 测试只断言文案内容，未覆盖渲染路径，en 默认语言
    掩盖了缺陷
  - [如何避免]：对用户可见/多语言字符串一律按字符切片（`chars().take(n)`）
    或先用 `is_char_boundary`/`floor_char_boundary`；涉及 i18n 文案的渲染
    逻辑，回归测试必须遍历 `Lang::ALL` 且执行真实渲染路径（TestBackend），
    不能只断言词表字符串
  ```

- **`CHANGELOG.md`** — `## [Unreleased]` → `### Fixed`: «TUI: shortcuts 栏在 ru/zh 下首帧 panic（字节切片落在多字节字符内部）».

---

## 12. Definition of Done

1. Тест 1 «красный» зафиксирован (вывод паники приложен), после правки — зелёный для `en`/`zh`/`ru`.
2. Тесты 2 и 3 (и 4 для варианта B) добавлены и проходят.
3. `grep` по crate подтверждает отсутствие небезопасных байтовых срезов (§8), результат зафиксирован.
4. `cargo fmt --all` + `make check` — зелёные (fmt, clippy `-D warnings`, tests, doc).
5. Ручной smoke `DEEPSEEKNOVA_LANG=ru|zh chat --tui`: первый кадр рисуется, shortcuts-строка видна, выход по `Esc Esc`.
6. Запись в `AGENTS.md` §5 и `CHANGELOG.md` внесена (по согласованию — §13).

---

## 13. Открытые вопросы к владельцу

1. **Вариант A или A+B?** A закрывает панику и не меняет вид строки; B реализует заявленное в комментарии выделение ключей (жирным только `Ctrl+P`, хвост серым) — это видимое изменение UI.
2. **Объём MR:** паника + сопутствующий мёртвый BOLD-путь в одном коммите, или B отдельным?
3. **`AGENTS.md` §5 / `CHANGELOG.md`:** вносить в этом же MR или отдельным документационным коммитом?

---

## 14. Порядок работ (сводка)

1. Тест 1 — воспроизведение паники (красный).
2. Вынести `shortcuts_spans` в чистую функцию, заменить байтовый срез на char-safe (вариант A).
3. Тесты 2-3 (+4 при выборе B); при B — довести выделение ключей.
4. Аудит остальных байтовых срезов crate (§8) — подтвердить и записать результат.
5. `cargo fmt` → `cargo test -p deepseeknova-tui` → `make check`.
6. Ручной smoke ru/zh.
7. По согласованию: `AGENTS.md` §5, `CHANGELOG.md`.
