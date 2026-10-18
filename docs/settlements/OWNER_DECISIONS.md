# Решения владельца: CP-058 / CP-059 / CP-068

**BLOCKED-ON-OWNER-DECISION — все ответы pending.** Рекомендации — **NOT admitted defaults**, не shipped PvP policy и не acceptance. Источник: `SOLARIS_SETTLEMENTS_LONG_TERM_ADDITION_SPEC.md` §23 (новые решения явно маркировать, численные значения фиксировать до gates; не выдумывать ради заполнения) и §15 (logout не отменяет начатый законный бой); прямое решение Main оставляет admission владельцу. Cursor — [IMPLEMENTATION_SPEC §1.5](IMPLEMENTATION_SPEC.md#15-a0-единый-словарь-ownership-и-текущий-курсор).

Формат ответа: для **каждого** вопроса «утвердить предложенное значение» либо точное другое значение; затем явное admission конкретного ruleset/revision. В матрице §1 каждая непустая ячейка — **отдельный** вопрос: «Утвердить `<definition>.<поле строки>` = `<ячейка>`?», не пакетное одобрение строки/предмета. `—` означает отсутствие вопроса. Источники чисел указаны у таблиц; units: HP, simulation ticks, blocks, условные mass units. Изменённые числа требуют новой proposal revision до gates.

## 1. RULESET-R2 — рекомендовать точные существующие proposal values

Источник всех ячеек: **CONTENT_SPEC §18.2**, не §18.1 приблизительные диапазоны. Сокращения: A = `arming_sword`, L = `longsword`, S = `spear`, B = `war_bow`, M = `mail_hauberk`, P = `pavise`; namespace `solaris-settlements`.

| Поле отдельного вопроса | A | L | S | B | M | P |
|---|---|---|---|---|---|---|
| base_damage_hp | 5 | 7 | 5 | 5 | — | — |
| damage_kind | cut | cut | pierce | pierce | — | — |
| minimum distance | 0 | 0 | 1.25 | 4 | — | — |
| reach | 1.9 | 2.8 | 3.2 | 24 | — | — |
| windup | 6 | 10 | 8 | 16 | — | — |
| active | 2 | 3 | 2 | 1 | — | — |
| recovery | 10 | 16 | 14 | 20 | — | — |
| stamina cost | 12 | 22 | 16 | 18 | — | 14 за блок |
| cone half-angle | 45° | 55° | — | — | — | — |
| thrust radius | — | — | 0.18 | 0.12 | — | — |
| max durability | 320 | 280 | 240 | 192 | 480 | 360 |
| wear | 1 за исполненную атаку | 1 | 1 | 1 за реальный выстрел | 0.5 за HP воздействия | 0.75 за HP блокированного воздействия |
| torso coverage | — | — | — | — | 1 | — |
| arms coverage | — | — | — | — | 0.75 | — |
| legs coverage | — | — | — | — | 0.2 | — |
| armor cost | — | — | — | — | 2 | — |
| mass | — | — | — | — | 8 | — |
| activation | — | — | — | — | — | 8 |

Исходные min–reach пары §18.2: `0–1.9`, `0–2.8`, `1.25–3.2`, `4–24`; исходные windup / active / recovery: `6 / 2 / 10`, `10 / 3 / 16`, `8 / 2 / 14`, `16 / 1 / 20`. Матрица лишь разделяет каждый выбор, числа не меняет.

| Один вопрос | Рекомендованное значение из CONTENT_SPEC §18.2 — NOT admitted |
|---|---|
| Какой carrier для этих definitions? | `minecraft:paper` |
| Какое правило item_model identity? | В точности полный ID definition |
| Какой max stack каждого definition? | 1 |
| Какой общий stamina capacity? | 100 |
| Какой stamina recovery за simulation tick? | 0.5 |
| Какое правило второй руки longsword? | Свободная вторая рука |
| Какое правило рук war_bow? | Обе руки |
| Какой ammo debit одного реального выстрела? | Ровно 1 настоящий `minecraft:arrow` |
| Какова arrow speed? | 1.5 блока/tick |
| Какова arrow gravity? | 0.035 блока/tick² |
| Каков arrow collision radius? | 0.12 |
| Каков arrow lifetime? | 160 ticks |
| Каков arrow recoverable? | true |
| Каков arrow impulse? | 0.18 |
| Какой mail equip slot? | chest |
| Каков mail resistance cut? | 0.45 |
| Каков mail resistance pierce? | 0.30 |
| Каков mail resistance crush? | 0.12 |
| Каков mail resistance fire? | 0.05 |
| Каков mail resistance blast? | 0.10 |
| Каков mail resistance hook? | 0 |
| Каков общий mail resistance cap? | 0.60 |
| Какова mail degradation point при remaining_fraction 0? | `(0,0.2)` |
| Какова mail degradation point при remaining_fraction 0.25? | `(0.25,0.6)` |
| Какова mail degradation point при remaining_fraction 1? | `(1,1)` |
| Какова pavise width? | 1.0 |
| Какова pavise height? | 1.75 |
| Каков pavise forward offset? | 0.55 |
| Каков pavise bottom offset? | 0 |
| Каков pavise hook disable interval? | 30 ticks |
| Каков pavise impulse limit? | 1.2 |
| Какое правило pavise custody при deploy? | Тот же count=1 stack со всеми instance components; один native world instance, не копия |

Фазы/recovery не отменяются сменой слота, reconnect/unload; расход/эффект только через native исполнение. Owner admission не заменяет player/NPC range/phase/coverage/block/deploy traces и ITEM-01–05/WAR-01–08. Шесть definitions не закрывают CP-068 CAT/ITEM/CAM или ≥650 minimum / 768 plan.

## 2. RULESET-CP059-SUPPLY rev 1 — отдельные вопросы supply

**Точный существующий proposal:** CONTENT_SPEC §18.3. Все ID recipes имеют prefix `solaris-settlements:`; значения ниже рекомендуются **только как pending proposal**, не mounted defaults. Каждая ячейка — отдельный вопрос «Утвердить `<recipe>.<поле столбца>` = `<ячейка>`?»; входы saddle разделены, чтобы каждое количество утверждалось отдельно.

| Recipe | Вход 1? | Вход 2? | Выход? | Station? | Simulation ticks? | Tool wear? |
|---|---|---|---|---|---|---|
| `saddle` | 3 `minecraft:leather` | 1 `minecraft:iron_ingot` | 1 `minecraft:saddle` | `minecraft:crafting_table` | 240 | нет |
| `hay_block` | 9 `minecraft:wheat` | — | 1 `minecraft:hay_block` | `minecraft:crafting_table` | 80 | нет |
| `open_hay` | 1 `minecraft:hay_block` | — | 9 `minecraft:wheat` | `minecraft:crafting_table` | 40 | нет |

**Mounted значения отсутствуют в этом proposal** (CONTENT_SPEC §18.3): supply не задаёт healing, stamina recovery, mounted collider/seat или charge. Для каждого поля ниже нужен отдельный ответ; рекомендуемая процедура — дождаться настоящего generic native profile и зафиксировать отдельный числовой mounted proposal для owner admission, **не придумать числа из saddle/hay recipes**.

| Один вопрос до mounted admission | Рекомендация — pending, не числовой default |
|---|---|
| Какой native feed item? | Конкретный настоящий inventory item из будущего mounted proposal |
| Каков item debit на кормление? | Точное количество из будущего mounted proposal; не виртуальный корм |
| Какой simulation-tick интервал feed consumption? | Явный native feeding profile; не wall-clock/downtime catch-up |
| Сколько HP даёт кормление? | Явный native профиль; не plugin HP и не вывод из 9 wheat |
| Какой stamina recovery от кормления? | Явный native профиль; не перенос rider capacity/recovery 100/0.5 на horse |
| Какой mounted collider profile? | Реальная native horse geometry; без pseudo-mount |
| Какой rider seat/binding profile? | Native horse/rider binding; не teleport-as-riding |
| Какой velocity threshold допускает charge? | Явный native charge profile, проверяемый WAR-10 |
| Какой damage profile у charge? | Отдельный числовой mounted proposal, не обычный melee damage |
| Каков charge impulse? | Отдельное числовое значение будущего native mounted profile |
| Какой brace-versus-charge profile? | Отдельное native взаимодействие пикового фронта, не автоматический bonus |

Supply conservation/cold replay остаются отдельным gate; они не закрывают WAR-09–13. До появления native контракта невозможно допустить конкретные horse/rider/carry profile values; owner ответ сам по себе этот контракт не реализует.

## 3. CP-058 — явный admitted political ruleset, не inferred PvP

Источник существующих границ: **CONTENT_SPEC §21**. `war_policy.rs:Policy.ruleset` прямо требует **explicit admitted political ruleset record, never a compiled default**. `PoliticalFacts` берутся из текущих политических records, не полей команды/UI. Вопросы без существующего числа имеют отдельно названную **новую рекомендацию**, rationale и остаются pending owner admission.

| Один вопрос | Рекомендуемое значение / политика — NOT admitted | Источник / rationale |
|---|---|---|
| Какой ruleset record явно допускает campaign war? | Новая рекомендация: `campaign-war-r1`, отдельный explicit admitted record | ID не предложен §21; одно проверяемое admission, не compiled default |
| Какая ruleset revision допускается? | Новая рекомендация: 1 | Первое явное admission; не ранее одобренная revision |
| Сколько известных lawful targets допускает одна war record? | Не более 8 известных законных целей | Точный proposal CONTENT_SPEC §21 |
| Сколько интервалов допускает одна war record? | Не более 8 интервалов | Точный proposal CONTENT_SPEC §21 |
| Какой max policy value size? | Не более 4096 UTF-8 bytes | Точный proposal CONTENT_SPEC §21; native storage bound не расширяется |
| Сколько новых damage/destruction commits разрешено после revoke/peace? | 0 новых damage/destruction commits | Точный proposal CONTENT_SPEC §21 |
| Какой точный `grace_end` допускается для declaration? | Рекомендованная политика: owner/campaign admission явно задаёт абсолютный simulation tick ≥ declared_at; численного default нет | Точный proposal CONTENT_SPEC §21; конкретный tick ещё требует ответа, не произвольного числа |
| Какой точный `start` каждого window допускается? | Рекомендованная политика: явный абсолютный tick ≥ grace_end и ≥ end предыдущего окна | §21 `[start,end)`; не recurring PvP calendar |
| Какой точный `end` каждого window допускается? | Рекомендованная политика: явный абсолютный tick > start | §21: default duration отсутствует; owner задаёт конкретный tick до gate |
| Какое offline-owner правило? | Новая рекомендация: `AdmittedConflictContinues` только для уже явно допущенного lawful conflict/target в текущем window; offline сам по себе не admission | Owner addition §15: logout не отменяет начатый законный бой; нет разрешения начать неизвестный offline raid |
| Какое private-target правило? | Новая рекомендация: отказ для `Private` | Отдельного owner policy нет; исключает raid как доступ к частному имуществу |
| Какое neutral-target правило? | Новая рекомендация: отказ для `Neutral` | Отдельного owner policy нет; сторонний субъект не становится целью чужой войны |
| Какое правило unknown protection/actor/target? | Отказ, без inferred rights | Точный fail-closed proposal CONTENT_SPEC §21 |
| Какие lawful goals допускаются? | Новая рекомендация: только `defence` и `recover_control` известных contested sites | Goal catalog не предложен; ограничивает первую admission защитой/восстановлением control, не free plunder |
| Какое политическое основание допускает target? | Новая рекомендация: current committed side/control record + доставленная lawful cause, observed_at ≤ delivered_at ≤ declared_at | Основание не задано §21; исключает UI claim/hidden target и устаревшее владение |
| Кто может declare war? | Новая рекомендация: current charter-authorized holder, committed declaration | Authority catalog не предложен; без admin/NPC proxy |
| Кто может end war? | Новая рекомендация: current charter-authorized holder по committed peace/surrender record | Authority catalog не предложен; конец действует на следующий commit |
| Кто может revoke war? | Новая рекомендация: current charter-authorized holder с durable revoke event | Authority catalog не предложен; отзыв не отменяет committed receipts |
| Кто может делегировать effect? | Новая рекомендация: current charter-authorized holder, явный bounded/revocable mandate | Owner addition §15 требует срок и отзыв; не неявные права |
| Когда campaign policy разрешает player target? | Новая рекомендация: только explicit campaign admission обоих политических actors, законная известная цель и native server PvP permission; иначе отказ | Player policy не предложена; не меняет server-only Minecraft и не устанавливает shipped PvP default |

Все новые political рекомендации требуют owner admission **до** effect gates; owner задаёт конкретные ticks и выбирает политику. Отсутствие ответа — не «автодопуск»: даже рекомендованный `AdmittedConflictContinues` не admitted default.

## Отдельные blockers и границы работы

- **CP-058:** owner admission выше **и отдельно** native build-commit guard (war/politics storage revisions checked in the same synchronous turn immediately before apply; a storage write landing inside that turn is not seen — core ADR 0009 «Build storage-version guards»); predicate/Permit не permission cache.
- **CP-059:** supply/mounted owner admission выше **и отдельно** generic **native horse admission / horse equipment/carry / rider binding / mounted control**. Ни предложение, ни supply receipt не реализуют эти endpoints.
- **CP-068:** RULESET-R2/itembalance admission выше **и отдельно** genuine **SDK catalog identity/hash query**; fixed expected-hash guard нельзя убрать/перепинить по build receipt. Полный каталог/acceptance остаются pending.
- Реализация независимых CP-050–057, CP-060–067, CP-069–096 продолжается в разрешённом scope; их существующие зависимости/evidence не меняются. Здесь нет новых PASS, core domain policy, guest code или разрешения обойти native blockers.
