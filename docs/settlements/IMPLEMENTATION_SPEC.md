# Solaris Settlements — техническая спецификация реализации

Статус: **draft · проект**, 2026-10-01. Автономная MMO-кампания встроена в существующие R/CP, без claim о готовности runtime или принятой gameplay.
Игровые требования: [SPECIFICATION.md](SPECIFICATION.md). Контент: [CONTENT_SPEC.md](CONTENT_SPEC.md).

## 1. Проверенная исходная точка

Историческая основа исследования 2026-09-15 (не текущий HEAD):

- `solaris-default-plugins`: `1d7db97266ea0d69ce0b530a5e4253977db20932`;
- `solaris`: `78b4f39d6a9363948c46f4983b243c9ce3fc97c9`;
- `solaris-loader`: `3aaa92663ce8bc3e7de2859ad40ed357aacf3382`.

Эти идентификаторы сохраняют происхождение, не статус текущего дерева. Чужие незакоммиченные изменения не сбрасывать. Сводка ниже обновлена чтением manifest/README/подключённых исходников 2026-10-01; builds/tests/клиентские сценарии в документационной работе не запускались.

### 1.1 Компонентный пакет

[plugin.toml](../../solaris-settlements/plugin.toml) объявляет `solaris-settlements`, version 1.0.0, API 0.7.0, entry `plugin.wasm`, события `operation.result`, `player.left`; текущие capabilities: `storage`, `player_queries`, `inventory_transfers`, `persistent_npcs`, `npc_work`, `npc_orders`, `world_structures`. Последние пять также required_features; worldgen selector отсутствует. Старые resident/world_sites/structure_operations имена — provenance, не текущая SDK authority.

**Client bundle уже объявлен:** schema 2, `settlements-ui`, artifact `client/settlements.zip`, manifest size 4478, SHA-256 `b8517c4e3f09833327d761c305e3a4607b9079d49272b16783185167d45598ce`, Fabric/NeoForge/Forge. Объявлены views/view_actions/world_previews/world_selection/assets/sounds и соответствующие permissions. Это прочитанные metadata, не новая проверка hash/доставки/рендера; полная gameplay/UI acceptance pending.

[sources/solaris-settlements/src/lib.rs](../../sources/solaris-settlements/src/lib.rs) подключает hire/demob/capture/combat/evacuation/demography/development/morale/view, production/technology/commerce/caravan и explicit paid-shipment runtime. Canonical registry-v2 использует `settlement-root-v2`, natural indexed immutable records и полный typed projection; v1 остаётся только явной offline migration/refusal границей. Product bounds 24 settlements/64 residents сохранены без поднятия native 4096-byte quota. Production schema-2 journal отдельно хранит bounded metadata/точные gross receipts, не inventory authority. Наличие путей — **in-code/unverified**, не accepted recurring economy, самостоятельные parties или campaign PASS; CP cursor §1.5.

Caravan storage (source-only, **in-code/unverified**): каждое durable значение ≤ 4096 bytes (core `MAX_PLUGIN_STORAGE_VALUE_BYTES`; бюджет записи 3840). `settlement-caravan-index-v2` — единственный ограниченный индекс id (≤ 64 маршрутов, `paid` фиксирован при admission, `open` — подсказка планировщика); контракт каждой перевозки хранится отдельно в `settlement-caravan-v2/{id}`, а его native receipts + pending intent — в `settlement-caravan-v2/{id}/ledger` (порядок записи: контракт, затем ledger; восстановление идемпотентно повторно применяет native receipt). Завершённые контракты никогда не вытесняются: replay identity operation id и committed gross-delivery receipts сохраняются. Admission и резервирование груза отказывают (без native effect), если worst-case размер контракта/ledger на core-bound идентификаторов не помещается; финальная проверка `value.len() > 4096` в пути записи сообщает вместо over-cap CAS. Native version guard (escort start/cancel/payout) адресует ключ контракта этой перевозки, а не общий store. Migration нет (single-value `settlement-caravans-v1` не читается).

README описывает original-UUID attachment/recovery, generic physical inventories, authored policy/native geometry separation и source-only ordinary-play/acceptance fixtures. Military item balance явно остаётся unapproved RULESET-R2 proposal; hire проверяет actual native age, не campaign elapsed time. Эти сведения не означают runtime acceptance или готовность native housing.

Текущий declared client bundle требует matching identities/permissions ACK перед Play. **Vanilla-client admission с этой декларацией не обещается**; server-only deployment без client declarations — иной scope, не fallback full product. Source widgets и динамические geometry/hash/revision в свежем build receipt не доказывают содержимое/activation принятого ZIP или реальный рендер; SDK runtime blueprint identity/hash query пока отсутствует, existing fixed gate expected-hash guard не снят и не перепинен.

Rust component source собирает core-Wasm → component `plugin.wasm`; `structures/` — authored TOML. Legacy Luau source — только porting provenance, не production runtime/альтернативный entrypoint. Новое состояние продукта/приёмка принадлежат plugin, не core.

### 1.2 Граница текущей поставки

Это исходная точка bounded команд/сохранённых социальных записей и declared client bundle, **не доказательство** принятой автономной кампании, государств, transit, scalable records или полного Loader UI. Текущий generic purge/API cutover не принят этим чтением. Реальная полнота проверяется на конечном дереве integration owner; ниже «есть в коде», «описано в проекте», «требуется» различаются.

### 1.3 Какие существующие решения использовать

Постоянные resident handles, intent/result/operation_status, CAS и storage_scan, bounded inventories/reservations, site survey, staged construction, work/order DTO, декларативные клиентские bundles и точную проверку SHA-256. Эти механизмы — основа, не конкурирующие версии новой архитектуры.

Не копировать `solaris-towns` в settlements: towns управляет игроками/землёй, а здесь NPC-общество. Не выдумывать cross-plugin money service. Не хранить фактическое содержимое склада одновременно в plugin ledger и chest.

### 1.4 Capability audit и оставшиеся требования

Пути core/Loader ниже относительны sibling checkout; номера строк не contractual. Это read-only аудит исходной точки, не утверждение об отсутствии возможностей во всех ветках.

| Возможность | Статус и прочитанное основание | Конкретное требование/граница |
|---|---|---|
| Rust/WASM 0.7 component + client declaration | **Есть в коде:** plugin.toml, README | Доказать deployment/совместимость/реальную UI-сцену отдельно; не объявлять full UI по ZIP |
| Постоянные social/hire/demob/capture/household intents | **Есть в коде:** lib.rs modules, SettlementRegistry/SettlementResident | Campaign parties/politics/payroll/autonomy **требуются**, не выводятся из команд/модулей |
| Registry 24/64, global CAS | **Есть в коде:** lib.rs `SETTLEMENT_REGISTRY_KEY`, `MAX_SETTLEMENTS`, `MAX_SETTLEMENT_RESIDENTS` | Локальные записи/indexes + offline миграция pending intents + измерение bytes/conflicts; поднятие констант не масштабирование |
| Inventories и fences | **Есть в изменяемом коде, перечитано после cutover:** `crates/mc-script/wit/inventories.wit` endpoints PlayerInventory, Container{dimension,position}, NpcEquipment/NpcCarry; `sdk/rust/solaris-plugin-sdk/src/inventory_ops.rs` query/reserve/transfer + query_inventory_reservation/release_inventory_reservation | Product warehouse names/bindings только plugin mappings → real Container, без core warehouse handles. Native ownership/protection/state/image/capacity/reservation fences сохраняются; final integration/runtime evidence pending |
| NPC identity/physical controls | **Исходная точка есть в коде:** resident/resident-order families и plugin commands; **согласованный target cutover, runtime proof pending:** NpcOperation/NpcOrderOperation, *_npc* SDK, query_npc_controls | Query identity/lifecycle и read-only controls не создают operation_id; Issue/CancelOrder/CancelWork replay по stable ID/fingerprint. Native controls — physical order/work/pending_work с revisions; roles/custody/morale/housing policy только plugin. Generic paid nonlethal ApplyHealthDelta связывает native health/material/receipt, не core лечебный ruleset; lethal effects идут existing combat. Party/Army parent и per-part outcomes plugin-owned |
| Settlement-named API | **Историческая исходная точка:** settlement DTO/imports; **текущий прочитанный caller:** `lib.rs` использует `world_structures` SDK | Clean generic cutover без aliases/reexports; окончательный purge/API proof у core integration owner |
| Certificate/warehouse функции | **Есть в текущем исходнике, unverified:** `lib.rs` BuildingOrigin, recognize/bind/check_worksite; `view.rs` live building page | Plugin policy/records/tests; native registered points/live checks. Runtime BUILD-05/07/ADD:ECO-02 pending, не PASS по файлам |
| Bounded views/actions/markers/input | **Есть в коде:** core client-presentation.wit; LoaderViewModel.java (wire 3 pages/actions/reasons/markers), LoaderWidget.java (schema 2 tables/input/world marker); plugin client-src/solaris-client.json | Dedicated known-data campaign map, filtered reports/delivery и early work/escort UX **требуются**; markers/overview не доказывают их готовность |
| Time/due/revision notifications | **Описано в проекте:** §§6.4/11 | Audit existing event/context/storage paths; durable deadlines/restart и missing producer notifications реализовать generic bounded API по потребителю |
| Work tickets, native transit/spatial activation | **Описано в проекте; требуется campaign proof:** §9 | Generic admission/budget/area/authority epoch/recovery + product admissibility policy; не выдумывать method names или достаточность имеющегося unload |
| Offscreen encounters/aggregate combat | **Требуется:** §9/§15.14 | Сначала physical tickets; aggregate допускается лишь после заранее определённого statistical gate, иначе measured physical cap |

Owner addition от 2026-10-01 и последующее прямое решение владельца разрешают **параллельную реализацию CP-049–096** и A0–A8; старые freeze/CP-ARCH/no-start CP-050 формулировки отменены. Приёмка и закрытие остаются dependency-ordered: generic purge/API → CP-049 actual BUILD-05/07 → последующие карточки. Ни наличие исходника, ни документационная A0 не закрывают runtime.

### 1.5 A0: единый словарь, ownership и текущий курсор

**A0 model/ownership alignment выполнено чтением и согласованием документов, не runtime-приёмкой.** Источники: owner addition §§4–10/21–22; SPECIFICATION §§0.2–0.3; CONTENT_SPEC admission/recipes; текущие `lib.rs`, `view.rs`, generic SDK `world_structure_ops.rs`. Исходные R/C/L/P/CP evidence ниже и в core roadmap сохранены; этот раздел — единственный live CP-049–096 status cursor, не альтернативный roadmap. Ни одной новой карточке не присвоен PASS. Builds/tests/client gates во время параллельных правок не запускались.

| Термин / факт | Единственный owner и текущая проекция | Пробел / связь |
|---|---|---|
| ActorRef / Resident | Plugin social alias хранит ссылку на **наблюдённый original AddEntity UUID** принятого WorldFeature; core один владеет handle/UUID/HP/death/position | `attach` не spawn и не замена человека; recovery после native admission сохраняет тот же UUID; A1/A2, CP-053 |
| Household / Settlement | Plugin households, rights и intents; canonical registry-v2/root + natural immutable record owners, product bounds 24/64 неизменны | Complete typed reads/opaque fields/optional absence; native value limit 4096 не поднят; v1 только явная offline migration/refusal, ADD:REC-01 |
| Building / Worksite | Plugin purpose/origin/certificate/binding и canonical `building_policy.json` (functions/capacities/restoration); native TOML содержит только generic geometry | Все 22 исходника до separation сохранены в `structure-policy-originals.tar.gz`; metadata не является physical capability; A0 runtime CP-049 |
| Stockpile / cargo / payment | Реальные Container{dimension,position}/NpcCarry/Equipment; exact native items/components/fences/reserves, никаких plugin inventories | Обычный fenced `transfer-cargo` не переписывает native work; active route требует native permit/epoch, пропуск permit не bypass; CP-050/052/053 |
| ProductionOrder / TransportContract | Plugin durable intent/result refs; core timed indexed work и **gross native portion receipts**; полный receipt сохраняется до ACK | Schema-2 root/pages/job heads/immutable fragments; migration-only v1, без alias/reset/catch-up; A1/A3, REC-04 |
| Party / Squad / Army | Party путешествует; Squad/Formation тактика; Army объединяет parties, не людей в новый roster | Native route executor epoch/exclusive lease и actual death cargo refs; реальные execution/recovery gates ещё не приняты; CP-053/056/087 |
| House/Clan / Realm / Treaty/War | Один политический House; dynasty genealogy его аспект; независимые war/trade/passage/vassal relations | Минимальная aggression/protection до рейда; A4/A7, CP-058/061/062 |
| Knowledge / Report | Plugin known source/observed_at/delivered_at; Loader только фильтрованный payload | Не выдавать скрытые inventory/roster; A3/A4, CP-054 |
| Physical / remote-ticket / transit / dormant | Native sole physical authority/exclusive executor; plugin admissibility и due policy | Dormant не производство; нужен реальный offline principal + admission, без admin/player proxy; A1/A2/A8 |
| Simulation time | Native tick и plugin durable due; не wall/sky time | Restart без downtime catch-up; ADD:TIME-01 |

Уточнённый CP-049 audit: generic live worksite/structure checks подключены в root status/view, production preflight и native проверку каждой timed portion; Committed не означает пригодность. Единственный crafting путь: durable production intent → actual timed native portions → точные gross receipts → полный durable journal → ACK. `certified_manual_workshop_refuses_damage_repairs_and_survives_unload_restart` теперь требует полную source-матрицу **station/entrance/container между portions для manual и authored**: ordinary survival Start/Stop mining, настоящий dropped-block pickup/hotbar swap, расход именно восстановленного repair item, fresh same-identity status, original UUID/independent order/native work ID, два полных immutable gross receipts, ACK/replay без нового эффекта и conservation после двух cold restarts. Замена carry input и заполнение output inventory используют реальные fenced transfers и тот же ordinary player station repair; debug give repair items удалён. Authored sawmill проходит настоящий Wasm Prepare/Fund/Advance и расход exact native material plan до этой матрицы. Это **source-only/unexecuted, не evidence/PASS**: BUILD-05/07/ECO-02, foreign/unload/restart/conservation и реальные client/UI screenshots остаются обязательными на конечном artifact. Generated village остаётся WorldFeature, не synthetic DurableStructure; capacities не берутся из product POI.

Registry-v2 operator source — `sources/integration-tests/src/bin/migrate_settlements.rs`: `--inspect WORLD --server-stopped`; `--dry-run|--apply WORLD BACKUP --server-stopped`. Dry-run использует disposable journal copy и create-new durable backup/plan с source versions/digests и actual NPC/order/operation audit. Apply допускает только matching-prefix resume, exact fullprojection/nativeaudit и final atomic root CAS + legacy deletion; без reset/ACK/fabricated receipts. Scope fence — actual `settlement-v2/<blake3("settlement:"+scope)>/head` и positive native version, не logical root surrogate. Fragments 3072 UTF-8, pages 8, shared publication 16 mutations/48 KiB. Эти source selectors **не запускались детьми**, runtime proof pending.

Narrow status audience source сохраняет существующие `family-status`/`development-status` pipelines и CLI output: actual result/failure добавляется в paginated existing Overview только тому же original view instance/authorized scope. Новых product models/native queries/screens нет; delayed reply не переносится в reopened/foreign view. Unknown housing/free-bed/conditional home commit остаётся явным prerequisite, не Ready и не DEM PASS.

Paid-shipment source retains distinct original registered recipient UUID/home scope and original four-scope guards; reserve recovery queries the original operation and reissues only on genuine NotFound under positive paid-head version. Full native endpoint-vector execution/independent offline principal remains blocked; guards/source presence are not payout proof. Runtime gate preview separately awaits a genuine generic catalog identity/hash query: build-receipt facts, a new literal or removed expected-hash guard do not replace it.


**Статусы:** `in-code/unverified` — прочитанный исполнимый путь, доказательства ещё нет; `implementing/unverified` — текущая отдельная доменная работа, не PASS; `required/unverified` — требование, здесь реализация/доказательство не подтверждены. `blocked proof` называет конкретную границу. Колонка acceptance обновляется только integration owner по реальному evidence на конечном дереве. Closure cursor: **CP-049**.

| CP | Результат / owner | Реализация | Acceptance / следующий gate |
|---|---|---|---|
| CP-049 | Player-built capability parity / plugins + generic core | in-code/unverified: original-UUID attachment, full manual/authored three-point between-portion source matrix, real survival repair/input/capacity/construction and immutable gross receipt/cold conservation assertions | pending BUILD-05/07 + ADD:ECO-02; execute final-artifact portions/foreign/interruption/unload/restart/conservation and inspect actual UI; no PASS |
| CP-050 | Real production chains / plugins | in-code/unverified: sole schema-2 production runtime, canonical native recipe contributions, exact receipt fragments, saved quality/tools/wear and REC-04 source fixture | pending after CP-049; actual manufacture/use, full gross receipts before ACK, CAS/crash/replay/native quotas and conservation |
| CP-051 | Local technology practice / plugins | in-code/unverified: local exact-practice/teacher transfer + complete schema-2 production receipt consumer | pending after CP-050; TECH-01–07 + real gated craft; unresolved catalog nodes remain visibly unavailable |
| CP-052 | Real emerald/barter/payroll / plugins | in-code/unverified: durable wages and native atomic physical trade + root/UI hooks | pending after CP-050; ECO-11/14, stale/last item/replay/restart/forbidden and explicit payment mandate |
| CP-053 | Physical caravan / plugins + native cargo | in-code/unverified: localized durable contracts, reserved cargo/native epoch/exclusive execution APIs and source fixtures | pending ECO-12/BUILD-08, actual observed/remote delivery/loss/death cargo/crash/revocation; A2/A3 and §22 vertical, no offscreen effect claim |
| CP-054 | Usable economic/military UI / plugins + Loader | in-code/unverified: seven exact native panes, filtered facts/recipes/stock/receipts, explicit terms + fresh confirmation, genuine Move/Stop before work; existing status pipelines → original overview audience | pending real final-artifact UI actions/rendering, rights/stale facts, two resolutions/scales and all declared adapters; new audience source unrun |
| CP-055 | Real content pipeline / plugins + validators | in-code/unverified: immutable candidate/native geometry/Loader provenance; canonical verify/deploy bind producer inputs, source revision, Core/SDK and exact recorded artifacts; actual wrong-directory and stale-preflight verify/deploy refusals observed before staging | pending fresh frozen-r2 SDK/guest/package/operator rebuild and actual activation/ACK/refusals/components/screenshots plus ordinary-play acquire→manufacture→use; guard smoke is not Native or gameplay acceptance |
| CP-056 | R2 complete retinue / plugins | in-code/unverified: actual hire/kit/melee/arrows/retreat/food/heal/demobilize/restart source fixture | pending WAR/ITEM/CLIENT/REC, two real observers, required crash windows, profiling and PERF-A |
| CP-057 | R3 multisettlement economy / plugins | in-code/unverified: two genuine native villages preserve all originals/blocks/containers/components through adoption/restart source fixture | full CP-048–055 dependencies still required; normal-play development/technology/DEM branches E1–E4/payment mandate/interrupted shipping/trade/restart, not a narrowed two-site PASS |
| CP-058 | One war/destruction policy / plugins | required/unverified | **BLOCKED-ON-OWNER-DECISION**: explicit admitted political ruleset/revision, grace_end/window ticks, offline/private/neutral/player protection, lawful goals/targets and declaration/delegation authority — [one-question-per-value decision sheet](OWNER_DECISIONS.md#3-cp-058--явный-admitted-political-ruleset-не-inferred-pvp); native build-commit guard landed (checked in the same synchronous turn immediately before apply, not atomic; core ADR 0009 'Build storage-version guards'); protection/window/revocation before damage, no inferred PvP defaults |
| CP-059 | Cavalry/mount lifecycle / plugins + core | required/unverified | **BLOCKED-ON-OWNER-DECISION**: RULESET-CP059-SUPPLY rev 1 recipe quantities/station/ticks/wear plus explicit mounted feed/healing/stamina/collider/seat/charge/brace values — [decision sheet](OWNER_DECISIONS.md#2-ruleset-cp059-supply-rev-1--отдельные-вопросы-supply); separately blocked on native horse admission/equipment/carry/rider binding/mounted control; supply is not WAR-09–13 or identity/cargo acceptance |
| CP-060 | Real pre-gunpowder siege / plugins + core | required/unverified | crew/path/input/window/breach + two clients |
| CP-061 | Titles/contracts/succession / plugins | required/unverified | POL/REC-08, holder/replay/restart |
| CP-062 | Paid political actions / plugins | required/unverified | POL-13–16, unknown target/forbidden/replay |
| CP-063 | R4/R5 joint conflict outcome / plugins | required/unverified | dependencies CP-058–062; actual siege + agreement |
| CP-064 | Late materials/equipment / plugins | required/unverified | reachable resources→kit→use→repair→trade |
| CP-065 | One paid shot outcome / plugins + core | required/unverified | GUN/REC-09 + two observers |
| CP-066 | Artillery/physical damage / plugins + core | required/unverified | crew/cargo/protection/damage/recovery |
| CP-067 | Ordinary Minecraft compatibility / plugins + core | required/unverified | all actual mutation sources + server-only control |
| CP-068 | Full authored catalogue / plugins | required/unverified | **BLOCKED-ON-OWNER-DECISION**: RULESET-R2 per-item damage/range/phases/stamina/durability/wear, arrow/armor/shield profiles and explicit itembalance admission — [decision sheet](OWNER_DECISIONS.md#1-ruleset-r2--рекомендовать-точные-существующие-proposal-values); separately blocked on genuine SDK catalog identity/hash query, fixed expected-hash guard unchanged; CAT/ITEM/CAM and 768 plan/650 minimum remain pending |
| CP-069 | R6/R7 campaign / plugins | required/unverified | normal-play late production/supply/battery/multiplayer |
| CP-070 | Transport-free native simulation / core | required/unverified | same native outcome without fabricated socket/session + actual client |
| CP-071 | Player simulation ownership / core | required/unverified | inventory/menu/pose/reconnect competing mutation |
| CP-072 | Nonblocking simulation I/O / core | required/unverified | slow disk/consumer, admission/durability/recovery |
| CP-073 | Spatial handoff / core | required/unverified | epochs/migration/stale effects/two clients |
| CP-074 | Revision-fenced compute / core | required/unverified | stale world/path/light/unload/queue |
| CP-075 | Slow-client isolation / core | required/unverified | bounded ordered mandatory effects + healthy player |
| CP-076 | WASM isolation after cutover / core | required/unverified | slow/trapping guest, reload/generations/protection |
| CP-077 | Real multiregion composed commit / core | required/unverified | stale participant/capacity/recovery/conservation |
| CP-078 | One architectural runtime / core | required/unverified | no old authority; independent build + native/client regressions |
| CP-079 | Real-game performance baseline / core | required/unverified | unchanged workload/hardware/latency/queues/RSS |
| CP-080 | Measured worldgen improvement / core | required/unverified, conditional bottleneck | same seed/geometry/workload before/after |
| CP-081 | Measured lighting improvement / core | required/unverified, conditional bottleneck | same edits/light/client result |
| CP-082 | Active AI/pathfinding cost / core | required/unverified, conditional bottleneck | moving/working/fighting same roster, p95/p99 |
| CP-083 | Inventory/storage contention / core | required/unverified, conditional bottleneck | viewers+worker/cross-owner/fences/conservation |
| CP-084 | Network publication cost / core | required/unverified, conditional bottleneck | same visibility and transaction/lifecycle order |
| CP-085 | Capacity/autoscale admission / core | required/unverified | cgroup/pressure/hysteresis/bounded accepted work |
| CP-086 | Actual capacity/overload / core | required/unverified | exact 20-player/view-8/4-vCPU/8-GiB + unchanged stricter gates |
| CP-087 | R8 full campaign handoff / plugins + core | required/unverified | early A2 preserved; full SIM/PERF/two observers/crash windows |
| CP-088 | Operator install/diagnostics / core | required/unverified | strict startup/config/grants/log/stop paths |
| CP-089 | Real storage crash recovery / core + plugins | required/unverified | filesystem decision windows, exact inventories/blocks/receipts |
| CP-090 | Disk/resource refusal / core | required/unverified | disk-full/permissions/torn tail; no silent rollback claims |
| CP-091 | Consistent backup/restore / core + plugins | required/unverified | separate restore, identities/items/status/manifest |
| CP-092 | Reproducible delivery/install / core + plugins + Loader | required/unverified | clean install/check/run and exact hashes/platforms |
| CP-093 | Explicit schema/content upgrade / core + plugins | required/unverified | supported migration dry-run/backup/verify; no shims/reset |
| CP-094 | Mixed long-running operation / core + plugins | required/unverified | actual jobs/battle/caravan/save/reload, no idle-soak PASS |
| CP-095 | Evidence denominator / integration | required/unverified | final-tree receipts/skips/failures, unchanged DoD denominator |
| CP-096 | Scoped release candidate / integration | required/unverified | dependencies + no-debug/multiplayer/recovery/owner gates; no publish authorization |

Crosswalk A1–A8 сохранён в §14.0; §22 addition — **ADD:** строки §15.14. A0 документационная часть отдельно завершена здесь; A0/CP-049 physical runtime остаётся pending. A1 самостоятельное хозяйство → CP-050/057 + offline tickets; A2 handoff → ранний CP-053/056 и полная CP-087; A3 trade → CP-052–054/057; A4 защита/разведка → CP-056/058; A5 совместная армия → CP-056/062; A6 осада → CP-058–063/066; A7 политические субъекты → CP-061–063; A8 scale/campaign → CP-069/079–087/089–096. Центральная §22 delivery/intercept вертикаль принимается совместно, не списком независимых mock successes.


## 2. Разделение владельцев

### 2.1 Core Rust — общие авторитетные механики

В `solaris`:

- vanilla worldgen, chunk/region ownership и реальные блоки;
- persistent entity lifecycle, identity, здоровье, инвентарь, equipment и mounts;
- выполнение физических работ, навигация, combat/projectiles, collision;
- world/inventory transaction и recovery;
- package validation, checked immutable data registries;
- approved views/input transport, permissions, target validation;
- bounded scheduling, materialization и измерения;
- engine invariant harness и generic engine tests.

Core предоставляет **только generic engine + широкие reusable APIs**, без settlement/economy/town/claims policy, social/economic ledgers, domain certificates, warehouse ownership law или first-party acceptance tests. Всё это, включая политические роли/законы и состояние/тесты, в plugins. Core проверяет native invariants и выданные полномочия, не выбирает цену/цель/дом/владельца склада. Generated vanilla villages — world facts; authored settlement policy не остаётся в worldgen и не создаёт synthetic DurableStructure.

### 2.2 Component-пакет — игровая политика

В `solaris-default-plugins/solaris-settlements`:

- settlement/resident social/household/Party/Army/House/Realm records и membership indexes;
- admission/bootstrap/принятие sites, domain building certificates и warehouse-binding decisions;
- экономика, цены, бюджеты, recurring wages/rations, production/transport contracts, due/dirty policy;
- обучение/социальная morale/права/военные цели/разведданные/наследование;
- bounded физические intents, authored blueprints и client models;
- **все product acceptance/integration tests**, включая BUILD/ECO/WAR/CAM/SIM и migration кампании.

Политика вызывает общие исполняемые механики; не loop `damage_entity` для боя,
не 500 `move_to` каждый tick и не генерация блоков из package на worldgen worker.

### 2.3 Loader — клиентская презентация

В `solaris-loader`:

- один проверенный контентный протокол и immutable activated registry;
- widgets, каталоги, формы, selection/preview, карта известных данных;
- equipment/entity/composite presentation, анимации, звук, LOD;
- переназначаемый ввод и контекст команды;
- Fabric/NeoForge/Forge adapters;
- реальные клиентские сценарии.

Клиент не владеет уроном, деньгами, tech unlock, приказом, стройкой, fog of war или результатом сделки. Он отправляет **намерение** и рисует принятый результат. Скачанный bundle не содержит произвольный Java/JS/Luau-код.

### 2.4 Зависимости репозиториев

Core обязан собираться без обоих siblings. Loader обязан собираться в своей workspace. Integration-gates явно получают siblings и deployed package. Build-time authoring tool не становится обязательным runtime-зависимым сервисом.

Новые state machines размещать в существующих доменных областях либо узких соседних модулях; корневые orchestration-файлы только маршрутизируют. Не вводить единый `FeudalGameManager` в `server.rs`.

## 3. Поставка и модули пакета

### 3.1 Один продукт, один owner

Итог — один `solaris-settlements` owner с server data + verified client content. Не разбивать 768 предметов на фиктивные плагины, чтобы обходить квоты. Несколько bundles допустимы для организации доставки, но их aggregate validation едина.

Существующий manifest уже объявляет bundle и попадает под Loader-ack contract; не выдумывать ещё несуществующий optionality flag или второй client decoder. Финальный расширенный контентный профиль внедряется только после compatible core/Loader cutover и actual evidence. Server-only отсутствие declarations не поддерживается как скрытый fallback полной игры.

### 3.2 Текущая структура авторских исходников

```text
solaris-settlements/
  plugin.toml
  plugin.wasm
  README.md
  structures/
  client/settlements.zip
  client-src/solaris-client.json
sources/
  solaris-settlements/
    Cargo.toml
    Cargo.lock
    src/lib.rs
```

`solaris-settlements/` — строгий deployable package. `sources/` не
развёртывается сервером; он собирает Rust raw Wasm и кодирует
`plugin.wasm`. Подключённые Rust domain modules и declared client artifacts — реальная исходная структура; не возвращать `main.lua` execution. Присутствующий bundle/required handshake не является accepted полным Loader UI.

### 3.3 Версии

Различать server API version, package version, bundle schema, transport wire, content revision, save schema и worldgen identity. Номер `0.6.0` не означает автоматическую поддержку всего нового.

Не назначать новый wire/schema номер только из этой спецификации: сначала сверить существующие ветки core/Loader, затем один согласованный version cutover во всех validators/adapters/fixtures. Неподдерживаемый feature отклоняется до Play; нет старого decoder параллельно «на всякий случай».

## 4. Принятие деревень и здания

### 4.1 Core bridge

Существующий `VillagePlanSource` остаётся источником генерации. Нужен публичный bounded site descriptor для фактически сгенерированной деревни: stable site id, dimension, layout revision, bounds, materialized pieces, known POIs, inhabitant identities и provenance.

**Два разных действия:** core сгенерировал деревню; plugin принял её в своё управление. Принятие не должно требовать plugin worldgen plan, который выключает core villages. Catalog owner runtime-зданий и генерационный selector разъединяются при необходимом cutover, без второго генератора.

Site enumeration не генерирует бесконечный мир: только known/registered bounded pages. До контакта игрока принятому нейтральному site нужен конечный bootstrap/provenance/admission, не новый roster и стартовый запас при каждом discovery.

### 4.2 Adoption operation

Предлагаемый доменный процесс:

```text
adoption intent
  → read site + residents + permission basis
  → reserve settlement identity and admission capacity
  → claim exact eligible resident identities
  → bind approved buildings/POIs/containers
  → commit settlement records
  → publish known overview
```

Adoption — plugin процесс по generic site/resident/worksite facts; существующая семантика claim/identity сохраняется при API cutover, без второго API «преврати ближайшего жителя». Resident/site intent восстанавливается; частично принятое поселение не fully operational до разрешения conflicts. Generated village facts не преобразуются в authored settlement/DurableStructure ради этого.

Идентичность жителей генератора не пересчитывается из их текущей позиции. Игрок может увести жителя; он остаётся тем же человеком, а не новой местной демографической единицей.

### 4.3 Здания игрока

Bounded **plugin-owned building certificate** ссылается на generic registered worksite: footprint, purpose policy, interaction points, expected world revision. Core проверяет реальные doors/blocks/станции/контейнеры/live path и native work constraints, но не выбирает «кузница/склад принадлежит settlement». Повторный объём не создаёт дополнительные capacity/workplaces в plugin policy.

Blueprint и ручной certificate — разные domain admission пути к тем же generic native functional facts. Purge переносит certificate status/warehouse ownership tests и решения в plugin; native physical invariants остаются core. Не alias прежнего settlement API.

## 5. Модель данных и единственная authority

### 5.1 Идентификаторы

`world_id`, `site_id`, `settlement_id`, `resident_handle`, `actor_ref`, `household_id`, `party_id`, `house_id`, `realm_id`, `title_id`, `squad_id`, `army_id`, `engine_id`, `operation_id`, `report_id`, `content_id` — opaque IDs/typed keys. Координаты и connection-local IDs не durable identity. Rust/WIT использует точные integer types; только числовые JSON/Loader fields ограничены safe integer range, UUID/hash остаются строками, u64 не проходит через double.

### 5.2 Основные plugin records

Все mutable records имеют schema, revision, identity, ссылку на owning scope и bounded fields:

- `Settlement`: site, political owner, size/stage/branch, specializations, policy refs, building/population index roots, treasury endpoint ref, economy cursor.
- `Building`: certificate/structure handle, capability set, condition, assigned work slots, inventory refs, project ref.
- `Household`: member refs, home, потребности/социальная policy, личные inventory refs; без health/позиции.
- `ResidentSocial`: exact core handle, household, age origin, culture/skills, civilian/military assignment, contract ref, training progress.
- `WorkAssignment`: resident, job definition, bounded area/station, input/output refs, priority, native work operation.
- `ProductionOrder`: recipe, quantity, material reservations, workshop, workers, committed work watermark.
- `Shipment`: source/destination, manifest refs, transport identity, route/cursor, escort, durable ownership mode.
- `TechnologyRecord`: scope, tech id, known/practised progress, source, workshop/project evidence.
- `Party`: самостоятельный ID, leader, distinct resident/actor refs, allegiance, assignment, route/geometry revisions, current order, execution-part/epoch refs, supply/cargo endpoints; не второй native HP/gear store.
- `Squad/Formation`: tactical member refs внутри parties, officer/doctrine/native group-order refs и per-member outcomes.
- `Army`: commander, operation/goal, joined party refs, assembly points, delegated authority, supply plan; без нового roster/treasury.
- `ServiceContract`: exact people/company, term, equipment provenance, wage/ration obligation, escrow refs.
- `EngineAssignment`: native engine handle, crew handles, commander, ammunition source, order refs; без второго ammo/HP.
- `House/Clan`: одна политическая организация с obligations/rights refs; dynasty/lineage/succession — её аспект, а не вторая организация.
- `Title`: holder, realm, territory refs, inheritance law, claims.
- `Treaty`: parties, obligation definitions, start/end tick, signatures, state.
- `War`: parties, goal, lawful engagement/destruction policy, siege windows, occupation refs.
- `OperationIntent`: kind, stable ids of child operations, fingerprint, reservations, last committed receipt, recovery state.
- `Knowledge/Report`: субъект/источник/observer, observed_at/delivered_at, uncertainty, location/content, carrier/recipient refs; не общий server-state view.
- `Route/Encounter/Siege`: проверяемые сегменты и события контакта; физические силы/цели/лагерь/подвоз/тактические задачи и условия исхода.

Потребности/навыки/социальная мораль — plugin policy; физические последствия применяются только через валидируемый native result. Если native combat поддерживает оперативную morale state, плагин хранит лишь doctrine/input/projection, не второй меняющийся meter.

### 5.3 Что живёт только в core

World blocks, container slots, item components, entity lifecycle/UUID, position, HP, equipment/carry/mount slots, projectile existence, active combat timing, collision, native work/order progress, reservation consumption, durable world effects.

Смерть отражается в plugin проекции по событию/snapshot. Удаление plugin record не воскрешает NPC. Namespace владения плагина не равно правам конкретного пользователя — оба слоя проверяются.

### 5.4 Инварианты

- Один живой человек имеет одну native entity identity и максимум одно активное основное assignment.
- Один предмет/компонент принадлежит одному physical endpoint или одному canonical travel cargo, но не обоим.
- `reserved = consumed + released + remaining` по каждому ресурсу.
- Количество людей меняют только рождение/смерть/вход/выход с provenance, а не materialization.
- Вассальный и genealogical graph не имеют циклов; один титул имеет одного текущего holder.
- Смена treaty policy не оставляет разрешение на последующий удар по бывшему противнику без повторной проверки.
- Unloaded не означает dead, missing inventory не означает empty, accepted не означает completed.
- Нельзя читать неизвестную противнику информацию через UI/action errors/карты, даже если она известна серверу.
- Household/home/citizenship/employer/location/military refs независимы; поход не стирает дом, один NPC максимум в одной active Party.
- Population/food/wealth/garrison/army_size — выводимые проекции; legal owner/holder/physical endpoint различны.
- Мобилизация прекращает местную работу, рацион применяется один раз; демобилизация возвращает выжившего с учётом травм/дома/вещей.
- Bootstrap конечен и однократен; неизвестная территория не создаёт бесконечную экономику/подкрепления.

### 5.5 Демография: reconciliation с current resident model (CP-048)

Сверено с живым core, не с прежним описанием.
Этот CP-048 анализ сохраняет evidence исходной identity/occupancy модели до generic purge, не требование оставить settlement policy в core. При cutover native identity/physical occupancy transition сохраняет UUID/handle; дом, семья, гражданство, миграционная policy и tests — plugin.

**Что пакет ведёт сам** (assignment/социальные связи — plugin, §6.1 спецификации; household — policy unit):

- `Household`: member refs (resident handles), дом (POI id), личные inventory endpoints, потребности/социальная policy. Без health/позиции.
- `ResidentSocial` на каждый core handle: household ref, `age_origin`, культура/навыки, гражданское/военное assignment, contract ref.
- Намерения и их recovery: `MigrationIntent`, `BirthIntent`, `RelocationIntent` — CAS-запись до native effect, проекция после authoritative result, как в §7.2/§7.3.

**Что берётся из core без изменений**: `Claim`, `Spawn { spawn_site_token }`, `SetPois`, `Release`, `Query`, `Treat`; resident handle/UUID/generation, lifecycle (`AliveLoaded | AliveUnloaded | Dead`), реальный инвентарь/экипировка, health, death. Одна занятая жильём единица = один consumed spawn-site token на одного жителя; `ReserveResidentSite` выдаёт token на generation identity.

**Чего сейчас нет и что нужно core** (блокирует переселение, DEM-01/§6.7): композиция существующих вызовов живого жителя не переселяет, и дело не только в occupancy. В текущей модели **идентичность выводится из generation identity**: `resident_handle_for_generation(plugin_id, generation_id)` даёт handle, `resident_entity_uuid(generation_id)` — entity UUID, `resident_spawn_site_token(plugin_id, generation_id)` — token сайта, а settlement-резервация отображает token на `poi_id`. Плюс `release_resident_site` возвращает `Blocked`, пока site consumed или привязан к рождённому жителю (тест `release_refuses_a_reservation_consumed_by_a_resident_spawn`).

Следствие: новый дом = другой generation identity = другой token, другой handle и другой UUID. Значит, «перенести личность и имущество» нельзя выразить ни свободным `SetPois`, ни связкой reserve/release: нужен один owner transition, который (а) освобождает прежнюю занятость, (б) потребляет token нового дома и (в) **переносит запись жителя на новую generation, сохраняя handle и UUID**. Последнее меняет правило вывода идентичности для переселяемых жителей и потому требует ADR-обновления и отдельного решения владельца: это изменение identity model, а не только occupancy. Пакет не добавляет для этого собственный reconciler/cleanup и не пересоздаёт жителя.

Пока это решение не принято, срезы CP-048, не зависящие от переселения (households, взрослая миграция, рождение, паузы по жилью/еде, смерть опекуна, проверки roster/POI), идут вперёд; срез «Preserve identity and property across relocation» ждёт решения.

**Время**: `age_origin`/due tick хранятся абсолютным owner tick; «сейчас» берётся из tick обслуженного callback (event context). Возраст и переходы выводятся лениво на уже приходящих событиях household/housing/food/notification; timer допустим только там, где контракт требует автономного перехода в точный tick и существующие события его не дают. Отдельная global demographic simulation не заводится (§11).

**Приёмка остаётся DEM-01/DEM-02 (§15.4) плюс shared `settlement-cargo`.**

## 6. Экономические и inventory API

### 6.1 Сохранить существующую семантику

Сохранять семантику actual inventory/storage WIT/SDK exports. Generic inventory SDK уже содержит `query_owned_inventory`, `transfer_owned_items`, `reserve_inventory_items`, `query_inventory_reservation`, `release_inventory_reservation`; query/release выражают existing native ReservationStatus/Release, не второй ledger. Старое proposed имя `inventory_reservation_status` не production export. `Container{dimension, position:{x,y,z}}`, `NpcEquipment`, `NpcCarry` — физические endpoints; warehouse/stockpile — plugin names/mappings, не core binding handle. Остальные transport/escrow/storage_batch_cas/storage_scan/operation_status требования сверять с exact exports, недостающий путь минимально расширять по реальному потребителю. Объявления source cutover не доказывают пройденный runtime gate.

### 6.2 Обязательное расширение endpoint coverage

Нужны все реальные пути:

- player ↔ warehouse;
- player ↔ resident equipment/carry;
- resident carry ↔ warehouse;
- resident carry ↔ station buffer;
- carrier cargo ↔ warehouse/station;
- engine ammunition/component slots ↔ authorised crew/cargo;
- declared escrow ↔ контрагент через физический settlement/payment процесс.

`actor_id` пользователя подходит ручной операции, но автоматической работе нужен **server-owned execution principal**, производный от admitted job/contract с worker identity и ограниченным мандатом. Не посылать фиктивный player_id=0 как superuser и не требовать online игрока, чтобы NPC унёс хлеб.

Проверяются: owner, доменная роль, конкретный worker/actor, reach, loaded state, endpoint revisions, доступность/резервы, полный vector переводов. Cross-region перенос сохраняет один recoverable decision; простой цикл отдельных take/give запрещён.

### 6.3 Рабочие станции и рецепты

Immutable native recipe/workstation predicates расширяют настоящий crafting/work executor; рецепты/выбор/экономические правила — plugin data/policy. Не создавать guest-side «результат появился».

Новые outcome fields: committed portion id, exact inputs consumed, exact outputs/materialized intermediate, wear deltas, resulting endpoint fences, pause reason, recipe revision. Handler получает только bounded агрегат/страницу, а не полную историю мастерской.

Текущий исполнимый источник provisional gameplay decisions — plugin-owned
`sources/solaris-settlements/src/production_ruleset.json`; определения native
рецептов — `production_recipes.json`. Root `configure` публикует
`production::authored_recipes()` через generic startup `work_recipes`; fixture и
реальный server startup принимают ту же contribution через native item/work facts.
Численные quality/rate/durability решения не дублируются в core или втором документе.
Native own due tick/immutable duration ведёт порции; plugin хранит exact gross
inputs/outputs/tool-wear/recipe revision/portion sequence до acknowledgment.
Диагностический `work-status` читает настоящие QueryControls, а не registry fence;
explicit stale `issue`/`haul`/`harvest` revision не заменяется автоматически.

Pending lead selectors: `settlement_component_haul::
certified_manual_workshop_refuses_damage_repairs_and_survives_unload_restart`,
`settlement_wasm_readiness::
settlement_vanilla_village_warehouse_funds_and_commits_through_wasm`,
`settlement_technology_commerce::
accepted_local_practice_restart_and_physical_trade_are_replay_safe`.
Фикстуры запускают shipped guest и actual server/native inventories; строки здесь
не являются executed proof. Lead запускает scoped debug/jobs2 gates последовательно.

### 6.4 Изменения состояния как события

Требуются owner-scoped notifications для inventory/reservation/work/order/combat/lifecycle/site/policy. В текущей документации work/order/combat notifications ещё отмечены как отсутствующие — полный overhaul не должен строиться на постоянном опросе всех NPC.

Event содержит producer sequence, identity, revision и typed delta. Событие — сигнал перечитать/применить проекцию, не отдельная экономическая authority. Потеря события восстанавливается snapshot с cursor; повтор не повторяет игровой effect.

## 7. Сохранение, транзакции и recovery

### 7.1 Durable operation

Одна операция получает стабильный operation id до эффекта. Повтор с тем же canonical fingerprint возвращает прежний результат; иной payload с тем же id — conflict. Request id коррелирует доставку, не дедуплицирует оплату после reconnect.

Важные состояния: rejected, accepted, running, paused, suspended, committed, cancelled. После возможного durable append нельзя объявлять результат отвергнутым только по timeout. UI показывает pending/recovering и запрашивает тот же id. Обратимая остановка работы использует generic `SuspendWork(original_admitted_operation_id, reason, expected_revision)`: opaque reason задаёт plugin, исходный id, planned/done, gross receipts и ACK сохраняются, due tick отсутствует, а `pending_work` продолжает показывать фактический остаток. Найм использует reason `military-service`; военный приказ не возобновляет приостановленное гражданское производство. `ResumeWork(original_admitted_operation_id, expected_revision)` допускается только для Suspended с актуальными actor/revision/station/recipe/quota fences, не создаёт новый Assign и не сбрасывает прогресс. `CancelWork` терминален: Cancelled не возобновляется. Runtime proof этого cutover остаётся за финальным lead gate; наличие исходника не означает PASS.

### 7.2 Трёхчастный протокол

1. Plugin CAS пишет намерение с identifiers будущих native operations.
2. Core выполняет world/inventory/entity transaction с единым recoverable decision и receipt.
3. Plugin CAS применяет результат к своим социальным/экономическим проекциям.

Это **не одна магическая транзакция двух хранилищ**. Crash между частями допустим и закрывается intent/reconciliation. Игровой атомарный эффект находится в core; прогресс интерфейса может отставать без повторной оплаты.

### 7.3 Конкретные recovery границы

- Найм: резерв оплаты/набора → смена assignment → выдача комплекта → подтверждение договора.
- Стройка: reserve → work portion + блоки + consumption + receipt → plugin projection.
- Ремесло: inputs → intermediate/output + wear + receipt, без второго output после restart.
- Выстрел: заряженные компоненты → projectile/shot receipt → завершение цикла.
- Смерть: entity terminal state → предметный loot/drop decision → социальные последствия.
- Наследование: один title/realm transition intent → изменение прав/обязательств → новые views; вещи не копируются.
- Travel materialization: handoff epoch → единственное представление людей/груза → committed acknowledgment.

### 7.4 Хранение и размер

Существующий bounded storage (старый контракт: значения 4096 bytes, 4096 live records; точные текущие константы перепроверить в capacity-checkpoint) не может без расчёта вместить 24×256 жителей, военный roster, семьи, 64 tech nodes и историю операций.

Нужен измеренный **generic storage capacity profile с campaign-потребителем**, не core campaign store и не отключение всех лимитов:

- проектная admission цель: 65 536 live records per campaign owner;
- существующий небольшой value bound по возможности сохраняется; длинные roster/history шардируются;
- native serialized storage budget: предварительно до 256 MiB, отдельно от Wasmtime linear memory/host allocations;
- открытые операции/резервы получают зарезервированную capacity;
- локальные owner records/index pages не загружают всемирный social graph в гостя;
- список/scan постраничный, mutation batch остаётся bounded; большая succession использует recoverable process, а не невозможный CAS всех тысяч записей.

Это proposed profile, который должен пройти нагрузку/восстановление. Простое увеличение `maximum_residents` в config не решает host limits.

Текущий global `settlement-registry-v1` не final scalable schema. Локализовать по settlement/party/contract/House; resident identity одна, membership — согласованные ссылки/индексы. Бounded due-queue и dirty-наборы, CAS revisions и сообщения вместо world mutex/всех пар NPC. До изменения registry — backup/dry-run/reference audit и pending operation migration, без silent reset имущества. Измерять bytes written/conflicts/recovery на изменении одного работника; 24/64 limits не повышаются только ради таблицы. Ни standalone DB/broker, ни workflow/event-sourcing framework без необходимости.

### 7.5 История и дедупликация

Не удалять receipts, если их удаление снова делает старую операцию допустимой. Для первой реализации оставить durable result index на диске, bounded cache в памяти, вывод истории постраничный. Рост хранения измеряется на длительном campaign soak.

Любая compaction обязана сохранить точную семантику replay/result/fingerprint; если невозможно, требуется явный новый контракт, а не замена старого outcome на guessed success. Не добавлять event-sourcing всей игры ради этого: нужны journaling существующих опасных переходов и bounded summaries для обычного UI.

## 8. Оружие, NPC и machines: native extensions

### 8.1 Custom item identity не равна модели

Ванильный carrier + `ITEM_MODEL` решает отображение, но не определяет урон, прочность, рецепт, экипировку и сохранение всех 768 вещей. Новый item registry должен связывать canonical owner id с checked profile и компонентами.

Проверить по всему жизненному циклу: inventory, cursor, hotbar, offhand, container, craft, trade, drop/pickup, death, entity equipment, save/restart. Нельзя потерять owner id на одном пути и получить обычный iron_sword, который затем размножается при обратной конвертации.

Никаких client runtime registry integers в durable save. Presentation map connection-scoped, canonical id постоянный.

### 8.2 Combat profile executor

Immutable definitions загружаются один раз. Native state machine исполняет windup/active/recovery, stamina, shield, armor coverage, projectile и mounted constraints. Profiles выбираются фактически экипированным предметом; client-supplied attack profile id проверяется как допустимое намерение, не полномочие.

Generic damage resolver применяет server PvP/protection/выданную текущую affiliation/doctrine policy. WASM получает committed event для plugin опыта/политики; bounded target/sweep predicates native, без core War/Realm laws.

### 8.3 NPC perception и command policy

Local spatial index, ограниченные candidate sets, LOS по настоящему миру, current policy revision. Старый target ref перестаёт действовать при expiry/смерти/смене союза/невидимости согласно виду приказа. Разведчик не превращает все сведения сервера в карту.

NPC реагируют на producer signals цели/пути/supplies/worksite/order/атаки. Нет сканирования всех поселений/отрядов every tick в guest.

### 8.4 Party, Squad и Army admission

Native bounded group admission проверяет exact handles/fences и выдаёт per-member результаты; нынешний plugin command README ограничивает march/retreat/halt 32 aliases, старый target batch 64 не означает доступность большего командного пути. Party membership/Army parent intent и command authority — plugin; assembly происходит в реальных местах, без копирования людей/предметов.

Army intent раскладывается в bounded per-party/per-squad операции; delegation ACL и expected order revision решают конфликт командиров. UI показывает applied/blocked/rejected parts, не глобальную атомарность. Dead/unloaded/migrating/captive/wounded member имеет явный outcome. Тот же permission path для player, NPC-lord и captain, без online-session/superuser подмены.

### 8.5 Mounted actors

Райдер и mount — две entities и один проверяемый binding. Native pathing использует размеры/движение mount. Mount death/dismount/region transfer не создаёт повторного всадника и не оставляет невидимый carrier. Экипировка коня имеет свой endpoint и provenance.

### 8.6 Composite machines

Закрытые engine definitions: collider shapes, hit zones, rig, sockets, crew work slots, inventory endpoints, allowed motion, deploy anchor, fire profile, dismantle outputs. Не универсальный scripting движущихся чанков.

Состояния машины и отдельного crew job согласованы: отсутствие наводчика может запретить aim, а отсутствие одного заряжающего замедлить допустимый процесс только по определению. Crew checks перед commit; смерть расчёта после shot commit не отменяет уже летящий снаряд.

### 8.7 Разрушение

Damage по зданию адресует bounded volume/section и canonical blocks. Проверить права войны и обычную защиту до commit. Структурный профиль определяет сопротивление и локальные группы collapse; не сканировать опоры всего мира.

В одном recoverable portion: changed blocks, salvage/drop, invalidate POI/path/collision, damage receipt. Клиентский debris purely visual, не второй источник материалов. Обычная кирка и снаряд идут через согласованный world mutation/invalidation, иначе получится невидимый целый дом с дырой в модели.

## 9. Фоновая симуляция без подмены мира

### 9.1 Режимы и обязательное обслуживание

- **Observed physical:** native entities/blocks/work/combat в union всех player и active projectile/pursuit/container interaction areas. Не camera-distance, не visual LOD.
- **Remote physical:** bounded generic tickets (owner/area/allowed operations/budget/reason) обслуживают допущенную работу и encounters без игроков, включая фактический crop growth service и travel time.
- **Strategic transit:** causal progression существующих actors/cargo по ревизиям маршрута, native exclusive owner и сохранённым ресурсным последствиям.
- **Dormant:** временно не готов к физической работе; это не принятая постоянная экономика. До deadline нужен ticket либо explicit budget refusal.

Минимальная service capacity для admitted поселений резервируется; развитие/транспорт/encounters делят остаток. Измеряются время до depletion, due/actual delay и причина blocked/wakeup. CPU-starvation не изображается игровым голодом; новые длительные admissions ограничиваются, emergency administrative pause явна и сохраняет state/метрику. Logout последнего игрока не останавливает campaign.

### 9.2 Причинный маршрут и encounters

Route использует known nodes и локальную native navigation, bounded spatial queries; cost учитывает груз/транспорт/access. Перед порцией изменяемого сегмента проверяются geometry/right revisions. Safety validation не раскрывает AI скрытого врага. Продвижение ограничено ближайшим событием (encounter/bridge/observed boundary/no ration/arrival); контакт считается по интервалам и протяжённости групп, не совпадению конечных координат.

Разрушенный мост/новая стена останавливают соответствующую часть колонны на проверенной точке, не teleport всю Party; ожидание/обход/возврат/помощь имеют причину. Нельзя одновременно принять стратегическую delivery и physical loot той же копии.

Первый удалённый бой — bounded **physical encounter ticket** того же executor. Aggregate combat позже, только вне physical влияния: same identities/gear/ammo/terrain/orders/casualties/retreat и порции с сохранённым seed. До допуска фиксируются tolerances распределений duration/loss/ammo/outcome и проверяется отсутствие систематического switch advantage; numerical identity не обещается. Failure оставляет измеренный physical encounter cap, не unverified autoresolve.

### 9.3 Exclusive handoff и ранний correctness/cost gate

`physical → preparing_transit → transit → preparing_physical → physical` — generic owner transition, не spawn/despawn. Зафиксированы exact actors/native HP/gear/cargo/mount state/positions, order revision, last execution simulation time, stable operation ID и authority epoch. Старый executor теряет право записи до допуска нового; native record остаётся authority даже без ECS-body.

Materialization останавливает causal progress на конкретном tick, проверяет terrain/occupancy/transport/груз/interaction union, затем открывает физическую сцену. Crash между стадиями восстановим; replay возвращает прежний результат, не второго исполнителя. Нет heal/reload/food/fatigue/work-time reset/RNG reroll при входе.

Ранний срез R1/R2 ограничивает размер/длину группы и измеряет **и корректность, и стоимость** безопасной материализации; CP-087/R8 сохраняет полную приёмку больших workloads, не монополию на первый handoff. Большая Party сохраняет ID и непересекающиеся execution parts: два игрока на концах получают реальную колонну, не тела у лидера/второй хвост.

Активные снаряды/преследование/передача вещей/бой запрещают parking затронутой части только из-за logout. Обычный подход инициирует подготовку заранее; разрешённый login/teleport сначала готовит сцену. Нельзя пройти cost gate вечной невидимой стеной, внезапным spawn армии или недоступной половиной осады; overload вне принятого масштаба диагностируется.

### 9.4 Ограничения исполнения

Без admitted native области запрещены block harvest/break/place, craft без live station/reserve, siege/PvP effects, изменение private endpoints. Физический work ticket выполняет их обычным executor и permissions, а не background формулой `farm × elapsed`.

Смена observed/remote режима не ускоряет work units/рецепт/growth/путь и не отменяет wear/расход. Stopped-server downtime не создаёт catch-up, dormant не выдаёт невидимый урожай. Per-operation cheaper slices допускаются лишь после отдельного доказательства побочных эффектов.

## 10. Клиентский протокол и безопасность

### 10.1 Handshake

Configuration: точная версия wire/schema → required feature set → явное согласие permissions → загрузка declared bundles → проверка size/hash/схемы/границ → native resource reload → immutable activation → acknowledgement → Play.

Отказ разрешений/несовместимость/нехватка обязательных carriers — понятное отклонение до игровых эффектов. Hash доказывает идентичность байтов, не доверенность сервера. Новые permissions повторно спрашиваются; старое согласие не даёт права arbitrary code.

### 10.2 Предлагаемый расширенный контентный профиль

Исходные цели для полного каталога, проверяемые native validators:

- до 2048 declared custom item definitions aggregate;
- до 8192 verified asset entries aggregate;
- до 256 MiB распакованных активированных asset bytes;
- общий compressed campaign content budget ориентировочно 128 MiB;
- до 4 MiB проверенного index per bundle; суммарный bound фиксируется числом допустимых bundles и aggregate registry caps;
- до 64 screens, bounded page models ≤64 KiB, ≤64 rows/page;
- architecture blocks преимущественно vanilla; custom blocks только для действительно новых взаимодействующих станций;
- первый необходимый carrier profile — до 64 заранее зарегистрированных block states/slots, распределяемых между ≤16 функциональными типами; точный state allocation рассчитывается до handshake.

Эти значения **не существующие лимиты**. Не изменять только MAX_ITEMS: проверяются index bytes, schema entry counts, aggregate assets, model dependencies, native carriers, ZIP limits и resource reload memory. Если 768 предметов плюс материалы выходят за budget, оптимизировать/обосновать profile, а не скрыто отбросить часть каталога.

Не регистрировать Java item/entity classes после freeze. Использовать существующую carrier/content identity стратегию и обобщённые presentation types. При невозможности конкретного блока/машины в пределах carrier архитектуры — реализовать нужный native adapter до объявления поддержки.

### 10.3 Views

Сохранить existing `open_client_view`, `present_client_view`, `close_client_view`, `begin_client_selection`, `cancel_client_selection`. Предлагаемые новые семейства widgets: technology graph, item comparison, known-world map, squad roster/formation panel, timeline/obligations. Они декларативные и ограниченные по данным; не WebView с выполняемым кодом сервера.

Model: view_instance_id, model_revision, content_revision, current page/cursor, whitelisted actions, typed fields, reason. Client action: exact view instance/revision, action id, sequence, bounded fields и selection context. Player/owner/session подставляются сервером.

Stale form не совершает действие по старой цене/правам. После смены содержимого поля/кнопки old action invalid; UI получает reject + актуальную причину/модель. Резервный повтор связан с прежней durable operation, а не новым наймом/покупкой.

### 10.4 Карта и выбор на местности

Сервер формирует только известные игроку map tiles/markers. Unknown chunk не скрытый HTML-элемент с координатами. Remote orders принимают собственную область разрешения, а не перепользуют unrestricted x/y/z.

Selection context содержит exact session/owner/view/action, dimension, allowed area, target kinds, policy revision и expiry simulation tick. Сервер проверяет real pose/LOS/reach или полномочие разрешённой стратегической команды. Карта и world raycast — два явно разных вида intent, не обход reach через другой UI.

Preview hash совпадает с authoritative blueprint/transform; отсутствующий asset/несовпадение не заменяется «похожим» домом. Красный preview сопровождается фактической причиной, а результат survey перепроверяется перед mutation.

### 10.5 Equipment/animation presentation

Native entity остаётся authority; presentation DTO содержит canonical definition ids, visual state, animation phase/tick origin и quantized pose deltas по необходимости. Сетевые сообщения не копируют всю экипировку каждый tick и не вычисляют один и тот же reference mapping на каждого viewer заново.

На клиентах общий platform-common renderer/validator; adapters отвечают за native registration/input/lifecycle. Предсказание допускается для собственного визуального input, не финального урона, ammo или occupancy.

### 10.6 Ограничение злоупотреблений

Bounded input/action rates; untrusted fields/ids/string length; no unknown owner; role check на каждом effect; permission revocation; stale sequence/context; reconnect/session isolation; asset path traversal/ZIP expansion/undeclared file/hash mismatch; denied data not leaked в error text.

Это обычная безопасность многопользовательского аддона, не произвольное расширение scope отдельной телеметрией/античитом. Не выполнять код и не выдавать клиенту файл/сеть ради анимации.

## 11. Время и расписание

Один монотонный campaign clock от simulation ticks; sky time отдельно. Все production/service/age/contract события имеют сохранённый due tick и producer-driven wakeup. Wall-clock sleeps/polling не используются.

Серверное shut down приостанавливает мир. Restart не начисляет прошедшие реальные часы продукции/еды/возраста. Административная ускоренная симуляция — явная harness-only операция с normal transitions, не системная перемотка календаря без consumption.

Сохранять absolute due ticks/last_applied_tick/sequence; due-queue/local dirty indexes запускаются по событиям и deadlines, не числу выполненных callbacks. Bounded slice продолжает сохранённый cursor без всемирного scan. Смерть/передача вещей/мандатов не coalesce в необязательную «последнюю версию». Wasmtime fuel/epochs не прерывают blocking host call: WIT bytes/calls, induced native work/path/tickets/inventory/disk/network имеют свои admission bounds.

## 12. Производительность: цели, не обещания

### 12.1 Почему текущих defaults недостаточно

24×256 гражданских = 6144 уникальных людей; плюс до 2048 отдельных военнослужащих = 8192 целевых entries. Мобилизованный гражданский переходит между категориями, не считается дважды. Families/buildings/orders/contracts требуют локальных bounded records; весь state не сериализуется на mutation одного работника.

В бою сложность определяется не только числом NPC: path replans, formation collision, projectiles, spectators и world destruction. Нельзя вывести 512-man battle из успешного теста 512 idle entities.

### 12.2 Нагрузочные сценарии

**PERF-A:** 80 гражданских +64 военных, два игрока, второй загруженный населённый пункт, одновременно haul/craft/build/combat; debug-прогоны доказывают поведение.

**PERF-B:** 512 военнослужащих суммарно, 8 орудий, 4 клиента, смешанные melee/ranged/cavalry, перемещение через два региона, разрушение секции стены, обоз. Один бой, полный реальный сценарий.

**PERF-C:** 24 поселения/6144 гражданских +2048 военнослужащих в roster, 32 сессии, минимум два одновременно активных независимых боя по 256 бойцов; остальные территории в разрешённых режимах, длительная экономика/сохранение.

**PERF-D:** весь фактически принятый каталог (≥650 personal military items) плюс обязательные материалы/машины в activated registry; список/поиск/отображение всех production-моделей, смешанные комплекты, packet bytes, heap/native/GPU consumption. Отдельная capacity-нагрузка на 768 definitions может использовать явно помеченные stress fixtures; она не требует 768 принятых production-карточек и не засчитывает fixtures в контентную квоту.

Производительность окончательно мерить explicit performance/release профилем; обычная разработка debug. Для целевых 20 TPS на зафиксированном reference host: server tick p95≤50 ms, p99≤60 ms на steady-state участке; latency приказа admission→наблюдаемая реакция p95≤250 ms без искусственной сети. Превышения/паузы считать failure, а не скрывать средним.

Это **проектные пороги**, не утверждение о достижимости на каждом клиенте/сервере. Reference CPU/RAM/GPU, seed, distance, сборка, число viewers и run duration обязательны в отчёте. Нельзя молча снизить NPC count, если сценарий не проходит.

Ранний gate: два поселения/точный roster/один караван/два игрока + handoff correctness **и cost**. PERF-B проверяет открытое поле и плотные ворота отдельно, восемь орудий после R6; PERF-C включает остальных без игроков у производств и минимум два одновременных боя. Добавляются activation stress (teleport/два конца/geometry invalidation) и длительный autonomous/recovery soak: p95 при растущем service backlog — failure. Зафиксировать CPU/RAM limits, lanes, warmup/window, competing load и отдельный client frame-time threshold. `living-world-scale` 40k villagers + 60k animals не доказывает 100k combat actors; большой distributed gate не доказывает плотный choke point. Расширение до десятков/сотен тысяч records/тысяч бойцов — отдельные будущие уровни без скрытого уменьшения исходной нагрузки.

### 12.3 Допустимые оптимизации

- immutable catalog once at startup, shared IDs без копирования definitions;
- regional ownership и spatial indexes;
- стабильные formation slots и общий маршрут отряда с локальным обходом;
- path requests по invalidation/goal changes, не each tick;
- nearest visibility subscriptions, delta replication, LOD rendering;
- native bounded work batch и producer events вместо per-entity guest ticks;
- ограниченные active tickets и сохранённая стратегическая identity.

Не начинать со смены ECS/распараллеливания всего сервера без измеренного bottleneck. Всякая оптимизация должна сохранить common-play behavior, указанную границу и evidence before/after.

### 12.4 Saturation

До создания поселения/найма/размещения проекта резервируются roster/storage/active job capacities. Full queue даёт explicit pending/rejected result и не частичную оплату. Существующие активные операции имеют возможность завершить commit/recovery; новый admission не вытесняет их в потерю предметов.

На клиенте снижение визуальной детализации не меняет хитбоксы/LOS. На сервере нельзя пропустить урон по кадру, потому что рендерный LOD выключил модель.

### 12.5 Обязательная наблюдаемость кампании

Operator и игрок (только разрешённая часть) видят причину остановки, consumed effects и событие/deadline wakeup, не скрытые foreign stock/Knowledge из debug UI. Сводки ограничены по owner/page/event, без нового полного сканирования ради метрики.

| Область | Required counters/distributions |
|---|---|
| Population | Unique living IDs, civilian/mobilized/wounded/captive, physical/transit/dormant, orphan refs |
| Economy | Committed source/production/consumption, available/reserved/in_transit/loss, shortages, depletion deadlines |
| Work | Due/dirty/processed/work units, blocked reasons, queue age, actual service без игроков |
| Handoff | Requests/duration/retries/stale epoch/rejections и фактические bodies, per-part mode/position |
| War/logistics | Readiness/supply/casualties/travel/order age, real encounter/breach events |
| Cost | CPU/wall time по stage/lane, WIT calls/bytes, bounded host work/path probes/ticket chunks/allocations |
| Persistence | CAS conflicts/bytes written/recovery backlog/pending intents/required receipt age |
| Network/client | Fan-out/view-delta bytes/outbound backlog; server tick отдельно от client frame time |

Correlate `campaign_tick`, owner/lane, resident/party/settlement и operation ID; verbose tracing bounded. Игровой shortage и infrastructure service miss различаются; хороший p95 при бесконечно растущем backlog не принимается. Fuel/epochs не прерывают blocking host call ([Wasmtime Config](https://docs.wasmtime.dev/api/wasmtime/struct.Config.html#method.epoch_interruption)); trap/yield не откатывают accepted external effect ([interrupting execution](https://docs.wasmtime.dev/examples-interrupting-wasm.html)). Regional parallelism не доказывает dense-scene throughput ([Folia overview](https://docs.papermc.io/folia/reference/overview/)); это ограничение модели, не перенос чужого benchmark.

## 13. Миграция и отключение

### 13.1 Новый мир — основной проверяемый путь

Новая campaign запускается на vanilla settlement generation с отдельным gameplay overlay. Генерационные параметры не изменяются только ради принятия village. На старте выбранные content/campaign versions фиксируются вместе с save.

### 13.2 Текущий bounded command package / registry v1

Нужна отдельная offline migration до cutover: backup, dry-run, report, проверка schema и identity, перенос существующих записей/ссылок/operation ids. Не менять старое имя на новое и считать этого достаточно.

Проверить каждый resident handle, structure ref, reservation и supply projection. Устаревшая supply projection не превращается в новый физический склад с товарами. Непривязанные NPC не заменяются ближайшими. Legacy create sites без корректной vanilla identity сохраняются как player-founded/authored sites, не переписываются как естественная деревня.

Если runtime worldgen identity несовместима, миграция явно отказывает и предлагает новый world directory. Никогда не перегенерировать существующие чанки поверх построек. В первой реализации полнота поддержанного migration набора определяется actual v1 schemas, а неподдержанное fail-closed с отчётом, не data loss.

### 13.3 Удаление аддона

Без подготовки старт мира с активными foreign definitions/assignments отклоняется с объяснением. Отдельная maintenance detach операция: остановить новые jobs, завершить/отменить по receipts активные, вернуть/сохранить gear, demobilize/release NPC, снять UI/owner handles, подготовить разрешённую замену custom content либо сохранить мир как требующий пакет.

Не удалять custom вещи молча и не превращать неизвестный снаряд в air. Оператор подтверждает destructive content conversion отдельно.

### 13.4 Content updates

Stable ids сохраняются; recipe/behavior revision фиксируется в начатой операции. Нельзя после restart списать новые материалы по старому заказу. Удаление definition требует миграционного mapping/обработки in-flight states; simple rename не допустим.

## 14. Этапы реализации: законченные игровые результаты

Этапы — **вертикальные поставки**, не повод назвать частичный путь полным overhaul. Никаких точных сроков без команды и оценки ассетов. Для каждого этапа обозначены зависимости и доказательство.

**Статус всех R0–R9 ниже: описано в проекте / acceptance pending**, если конкретная карточка/evidence отдельно не доказывает свой bounded исход. Ранее завершённые P/CP receipts сохранены. Реализация CP-049–096 разрешена параллельно; **приёмка** dependency-ordered: purge/API → CP-049 BUILD-05/07 → последующие карточки. Единственный live cursor — §1.5, не A-метки.

### 14.0 Встраивание owner addition A0–A8 в R/CP

A-метки — только соответствия внешнего addition, **не новые stage IDs/roadmap/cursor**. Каждый результат остаётся частью существующего R/CP. Policy state и все product tests — plugins; core/API/native и Loader owners дают только общие возможности/презентацию.

| Соответствие / статус | Наблюдаемый результат и зависимости | State owner / repositories | Выход и неподдержанные случаи |
|---|---|---|---|
| A0 → текущий purge/API + CP-049; model alignment завершён §1.5, runtime pending | Один ownership/словарь; generic API и ручное здание работают/ломаются по факту | Plugins certificate/binding/acceptance; core generic worksite/world/inventory/live checks | Нет core policy/shims; BUILD-05/07 pending. CP-050 реализуется параллельно, но не принимается раньше зависимостей |
| A1 → R0/R1, CP-001–014/048–050; требуется | Самостоятельные жители получают работу, растят/добывают/доставляют/потребляют без owner online; зависит от generic work/inventory/ticket/time | Plugins jobs/households/needs/bootstrap; core physical authority; Loader minimal overview | Замкнутый цикл, shortage/restart/ECO/DEM/SIM; нет пассивного income, infinite bootstrap или «живой» dormant freeze |
| A2 → ранний R1/R2/CP-053/056 handoff slice; полнота CP-087/R8; требуется | Одна Party идёт с теми же людьми/грузом к двум наблюдателям; зависит от A1 identity/route/native epoch | Plugins Party/route/intent; core transit/activation/recovery; Loader per-part statuses | ADD:SIM-01–07, INV, cost gate; bounded length/size, не arbitrary massive army или дешёвая сериализация вместо materialization |
| A3 → R1/R3, CP-052–054/057; требуется | Два поселения, escrow/караван/доставка либо потеря; оплаченная работа и escort, known-data map | Plugins contract/economic policy/Knowledge; core stock/cargo effects; Loader early role/map views | Центральный сценарий §15.14 + ECO-11/12, INFO; A1/A2; без страховки/виртуальной валюты/cross-plugin services/omniscience |
| A4 → R2, CP-044–047/056 + ранняя policy R5; требуется | Охрана/разведка/рейд с ущербом допустимой цели; зависит A3/minimal sides, War/aggression/end/protection до рейда | Plugins hire/allegiance/War/reports; core physical combat/permission mandate; Loader commands | WAR/INFO/SEC + ADD:WAR-01/02; ограниченная тактика, не полный diplomacy/cavalry prerequisite |
| A5 → R2/R5, CP-056/062; требуется | Parties сходятся в настоящих assembly points, снабжаются, выполняют цель и расходятся | Plugins Army/command contracts; core bounded group/cargo; Loader per-party outcomes | A4, WAR-12/13, ECO mobilisation/demob; failure commander/wagon/late vassal явны; нет new roster/treasury/global atomic order |
| A6 → R4/CP-058–060/063; артиллерия R6/065–066; требуется | Лагерь/подвоз/вылазка/пролом либо surrender, occupation/restoration | Plugins Siege/rights/windows/capture policy; core world/engine/projectile facts; Loader scene | A5 + соответствующая R4 техника, SIEGE + ADD:WAR-03–05/REC; не таймер/road-only absolute blockade/free population reset |
| A7 → R5/CP-061–063; требуется | NPC rulers/House/Realm ограниченно выбирают цели/договоры/war end из известных причин | Plugins rights/treaties/House-dynasty/institutions; core effects, Loader diplomacy | A4 minimum policy развивается, R3/R4 resource/delegation; POL/SEC/REC; не all-pairs social graph/admin NPC/per-respawn succession |
| A8 → R6–R9/CP-064–069/079–096; требуется | Полная автономная MMO-кампания, content/cavalry/artillery/dynasty + несколько battles, migration/update/detach | Plugins product state/tests; core generic capacity/metrics/invariant gates; Loader all adapters | Существующие R dependencies + SIM/PERF/MIG/CAM/ADD; aggregate только после допуска. Не 100k combat/бесконечный мир по мирному core benchmark |

Минимальные UX/known-data map/diagnostics и cost proofs растут вместе с A1–A4, не ждут R8/R9. Финальные 768-plan/650-minimum, technology X–XV, cavalry/artillery/dynasty commitments не отменены и не prerequisite A2/A3; ранние representative items используют окончательный real content path.

### R0 — совместимый клиент и одна принятая ванильная деревня

Зависимости: нет новых этапов; использовать текущий core village path и существующий контракт.

Результат: package с согласованными core/Loader schema/permissions подключается на трёх adapters; из существующей ванильной деревни получается управляемое поселение с теми же NPC/блоками, рабочим обзором и одним подтверждённым содержимым склада. Runtime-каталог не выключает vanilla generation.

Закрывает: strict package/discovery, startup split, site bridge, minimal overview, exact identity/inventory presentation, no-Loader rejection. Evidence: ACC-01–06, CLIENT-01–03, REC-01.

### R1 — жители строят и обеспечивают себя

Зависит от R0.

Результат: поле/лес → склад → еда/материалы → реальная поэтапная постройка → заселение/работа. NPC↔warehouse и server-owned worker principal; ручное здание игрока валидируется тем же capability-путём; shortage/blocked/restart корректны. Приезд взрослых и последствия дефицита сохраняют личности, имущество и фактическую вместимость.

Не объявлять успех по выполненной `assign_work` без harvest/drop/chest result. Evidence: ECO-01–10, BUILD-01–05, DEM-01/05, REC-02–04.

### R2 — полный боевой цикл дружины

Зависит от R1; небольшой набор **настоящих** representative items использует окончательный profile/presentation path, а не временные fake swords.

Результат: нанять реальных работников, выдать разные kits, обучить, провести melee/ranged бой, отдать retreat, вылечить, демобилизовать с возвратом тех же предметов/людей. Мораль/плен и per-squad command UI работают.

Evidence: WAR-01–08, ITEM-01–05, CLIENT-04–07, REC-05–06, PERF-A.

### R3 — хозяйственное развитие и технологии X–XIII

Зависит от R1; военные unlock интегрируются с R2.

Результат: локальная практика/учителя/технологии, натуральная экономика/рынок, мастерские и цепочки, ветки manor/fortress/town без потери деревни, несколько поселений и физический караван. Рождение, взросление и переселение семей сохраняют конкретных людей и их имущество. E1–E4 family coverage, каталоги/сравнение/русская и английская локализация.

Evidence: TECH-01–07, DEM-02–04, POL-01, ECO-11–14, BUILD-06–08, CLIENT-08. Нельзя закрыть технологию только установкой boolean без произведённого предмета.

### R4 — кавалерия и предпороховая осада

Зависит от R2/R3.

Минимальные allegiance/агрессия/War goal/end/protection обязательны **до первого рейда R2**, а не впервые в R4. До разрушения R4 дополняет ту же plugin War policy объявлением, осадным окном/grace period и общей effect permission. R5 расширяет эти records основаниями/договорами, не создаёт вторую authority и не помещает War в core.

Результат: реальные mounts/корм/спешивание, контрвзаимодействие с пиками, путь машин, таран/лестницы/метательная машина/вылазка, пролом и оккупация по policy. Остальные инженерные конструкции доводятся через тот же production path.

Evidence: WAR-09–13, SIEGE-01–06/08–09, REC-07. Проверки запрета разрушения и offline-окон обязательны уже для первой осады; в R6 они дополнительно покрывают артиллерию. Полный PERF-B с восемью орудиями относится к R6/R8, не заявляется пройденным до появления этих орудий. Нельзя заменить реальную осаду summary screen.

### R5 — государство, дипломатия и наследование

Зависит от R3; применение войны к армии требует R2/R4.

Результат: нейтральные общества, realm/title/dynasty, council delegation, treaty, вассальная служба, war goal, мир, пленные, восстание из реальных людей и восстановление законной власти. Подкуп, спорная/поддельная претензия, поддержка претендента и физический саботаж имеют учтённые затраты и последствия. Player respawn не дублирует наследственное имущество.

Evidence: POL-02–16, WAR-14, SEC-01–06, REC-08. Династический экран без перехода власти этот этап не закрывает.

### R6 — экономика и война XIV–XV веков

Зависит от R3/R4; political war policy — от R5.

Результат: поздние металл/броня/ремесло, огнестрел, полный игровой пороховой цикл, 12 артиллерийских конструкций с crew/transport/reload/projectile/destruction, осадная батарея и снабжение.

Evidence: TECH-08–10, GUN-01–10, SIEGE-07–10, REC-09–10. Показать цепь от доступного сырья до выстрела и реального пролома.

### R7 — полный ассортимент и сквозная кампания

Зависит от R2–R6; ассеты отдельных семейств могут производиться раньше после фиксации соответствующих contracts.

Результат: ≥650 принятых personal military items при плане 768, все обязательные семейства/эпохи/здания/рецепты/машины, все источники доступны в обычной игре, нет дублирующих перекрасов/анахронизмов/неиспользуемых рецептов.

Evidence: CAT-01–12, ITEM-06–08, CAM-01–04. Нельзя объявить R7 только по размеру JSON registry. Баланс проверяется разнообразными начальными мирами/школами, не одной идеальной деревней.

### R8 — дальние походы, масштаб и безопасное обновление

Зависит от R1/R2/R5/R6 и representative full-size registry из R7.

Результат: полная масштабная приёмка уже введённого раннего handoff, многопоселенческая автономная кампания с localized records, минимум два активных боя при обслуживании остальных, длительный save/restart, supported registry v1 migration, detach/update, все Loader platforms. Первые correctness/cost переходы не откладываются сюда.

Evidence: SIM-01–08, PERF-A–D, MIG-01–04, CLIENT-09–12, CAM-05–06. Маленький успешный тест не уменьшает согласованный workload.

### R9 — окончательная игровая приёмка

Зависит от R0–R8.

Результат: владелец проходит обычную кампанию/целевые field-testing сценарии; закрыты найденные blocking common-play баги; varied-seed real-client exploration и один bounded adversarial pass; точная evidence matrix всех обязательных требований.

Итоговая маркировка по core Definition of Done: draft/stabilization/release-ready только по фактическим доказательствам. Пропущенный клиентский gate остаётся pending/blocked. Реальный провал владельца закрывается тем же или более строгим real-client сценарием, не зелёным Cargo.

## 15. Реестр приёмочных сценариев

Ни один сценарий ниже **не выполнялся в рамках написания спецификации**. Это задания будущему canonical harness; новые сценарии/параметры сначала должны быть реализованы и объявлены в manifests. Нельзя запускать несуществующее `run settlements` и считать его названием уже готового профиля.

### 15.1 Основа — ACC

- **ACC-01:** без плагина vanilla villages продолжают генерироваться; с runtime-каталогом и settlements-adoption не исчезают и не удваиваются.
- **ACC-02:** принять деревни plains/desert/savanna/taiga/snowy на заранее сохранённых seed/coords; сохранить существующие blocks/UUID.
- **ACC-03:** повторить discovery/adopt/reconnect/restart и проверить единственный site/roster.
- **ACC-04:** чужой owner/claim запрещает недопустимое присвоение, ничто не списано.
- **ACC-05:** удалённый/перемещённый житель не заменён ближайшим; пустой POI не создаёт гражданина.
- **ACC-06:** пакет с отсутствующим feature/asset/hash, вторым owner или неизвестной схемой fail-closed до Play.

### 15.2 Хозяйство — ECO

- **ECO-01:** работник с инструментом собирает реальное поле, доставляет output в наблюдаемый контейнер.
- **ECO-02:** семенной резерв/посадка расходуются; без семян новый цикл не запускается.
- **ECO-03:** дерево рубится в зоне; соседний дом из тех же logs остаётся цел.
- **ECO-04:** шахта удаляет реальные блоки, не получает скрытую руду и не продолжает через запрещённый участок.
- **ECO-05:** животноводство/рыбалка используют реальных животных/водный участок и объявленные входы.
- **ECO-06:** отсутствие food/tool/input/storage/route даёт разные видимые pauses; восстановление producer event возобновляет задание.
- **ECO-07:** полные destination slots не уничтожают груз, transporter остаётся владельцем.
- **ECO-08:** source/destination observers видят один transfer; соседний игрок не может забрать зарезервированный stack.
- **ECO-09:** player offline не превращается в фиктивный superuser для NPC; authorised worker продолжает только допустимую active работу.
- **ECO-10:** крафт consumes exactly inputs, создаёт output/intermediate once и сохраняет wear/quality.
- **ECO-11:** налоги/жалованье переносят существующую валюту; бедный household не печатает платёж.
- **ECO-12:** дальняя торговля требует груз/контрагента/доставку; уничтожение обоза оставляет фактический loss/loot.
- **ECO-13:** split/merge/перенос не омолаживают еду/пороховую партию и не снимают reserve.
- **ECO-14:** repair/salvage/trade roundtrip не создаёт material/currency прибыль без реального внешнего входа.

### 15.3 Стройка — BUILD

- **BUILD-01:** survey/preview/quarter-turn на границе чанков совпадает с placed structure, двери/лестницы/кровати целостны.
- **BUILD-02:** реальные builders и зарезервированные материалы дают поэтапное visible строительство.
- **BUILD-03:** pause/cancel после portion возвращает только remaining; уже поставленные блоки сохраняются.
- **BUILD-04:** игрок изменяет footprint после survey; проект не затирает его работу.
- **BUILD-05:** собственное здание игрока с годными входами/станциями выполняет ту же функцию; невалидное объясняет отказ.
- **BUILD-06:** манор, крепость и город строятся как районы на отдельных сохранениях без удаления деревни.
- **BUILD-07:** разрушение station/двери/склада немедленно инвалидирует функцию/путь; repair восстанавливает по факту.
- **BUILD-08:** дорога/мост влияет на реальный маршрут; сломанный мост прекращает прежний путь.

### 15.4 Технологии — TECH

- **TECH-01:** рецепт недоступен без parent nodes и фактической местной практики.
- **TECH-02:** трофей позднего оружия можно носить, но он не открывает массовый late crafting.
- **TECH-03:** teacher/документ/договор действительно переносит знание между местами с учтённой операцией.
- **TECH-04:** потеря станции прекращает production, но не стирает историю освоения.
- **TECH-05:** восстановление prerequisites возобновляет проект без повторного расхода завершённых portions.
- **TECH-06:** граф ацикличен, все unlock refs существуют; каждый gate достижим в default campaign без debug.
- **TECH-07:** сон/изменение sky time/restart не создаёт tech progress и взрослого рекрута.
- **TECH-08:** E5 пороховая ветвь открывается через свои знания/материалы, не один creeper drop.
- **TECH-09:** E6 арсенальный заказ требует реальную сталь/оснастку/специалистов/логистику.
- **TECH-10:** start-at-XV scenario содержит согласованные люди/здания/запасы; не только era flag.

### Демография — DEM

- **DEM-01:** взрослый иммигрант приходит из объявленного источника по допустимому пути, занимает свободное жильё и сохраняет один resident id/набор имущества; повтор прибытия не создаёт человека.
- **DEM-02:** обеспеченная семья с местом получает одного ребёнка через одну spawn operation; без условий рождения нет; ребёнок не допускается к военной службе.
- **DEM-03:** достижение взрослого возраста по campaign clock меняет допустимые назначения того же NPC, а не создаёт замену. Проверка ускоряет только нормальные simulation transitions в harness; sky-time/reconnect не заменяют прожитое время.
- **DEM-04:** переселение семьи между поселениями освобождает исходное жильё и занимает новое с единственным roster/имуществом; блокированный путь и restart сохраняют её в определённом состоянии без копии на обоих концах.
- **DEM-05:** нехватка еды/жилья даёт объявленные предупреждения и переходы потребностей; восстановление снабжения прекращает ухудшение, а терминальный исход фиксируется один раз. Просроченное жалованье само по себе не убивает человека.

### 15.5 Вещи — ITEM

- **ITEM-01:** одноручное/двуручное/копьё имеют разные observable range/timing/use, проверенные игроком и NPC.
- **ITEM-02:** shield orientation/armor coverage меняют результат правильным образом; нет защиты спины фронтальным щитом.
- **ITEM-03:** канонический id/components сохраняются на всех inventory lifecycle путях.
- **ITEM-04:** unload/reconnect/slot-switch не сбрасывают recharge в бесплатный выстрел.
- **ITEM-05:** павеза не существует одновременно как установленная и carried вещь.
- **ITEM-06:** полный accepted inventory ≥650, без quality/dye duplicate counting.
- **ITEM-07:** все required item profiles имеют обычный recipe/trade/loot источник и точную tech привязку.
- **ITEM-08:** визуальная проверка mixed armor/left-right hand/mount на всех adapters, без критического clipping и невидимой экипировки.

### 15.6 Армии — WAR

- **WAR-01:** два игрока одновременно нанимают одного NPC: одна служба/оплата.
- **WAR-02:** снятый с поля ополченец действительно больше не производит урожай.
- **WAR-03:** набор комплектуется из реального инвентаря; голый archer не стреляет без лука/ammo.
- **WAR-04:** follow/move/hold/patrol/garrison/attack/retreat выполняют разные заявленные задачи; после retreat нет старого chase.
- **WAR-05:** узкие ворота и изменение terrain дают перестроение либо понятный blocked, не teleport/stack.
- **WAR-06:** стрелок не наносит damage через стену и расходует ammo, снаряд наблюдается.
- **WAR-07:** потери/фланг/командир изменяют morale; routing и rally имеют физический результат.
- **WAR-08:** dismiss возвращает того же NPC и выданный gear; full destination оставляет demobilizing без потери.
- **WAR-09:** mount/dismount/death/transfer сохраняют одну лошадь/всадника/экипировку.
- **WAR-10:** charge требует реальный разгон; brace пикового фронта влияет на взаимодействие.
- **WAR-11:** cavalry не телепортируется через недоступный проход, horse feed расходуется.
- **WAR-12:** army order на много squads показывает частичные admission failures, не обещает глобальный all-or-nothing.
- **WAR-13:** medic/transport раненого сохраняет identity; пленный не числится одновременно в двух армиях.
- **WAR-14:** прекращение войны/смена союза запрещает следующий недопустимый удар даже при старом UI/order.

### 15.7 Артиллерия — GUN

- **GUN-01:** обычная цепочка производит первый игровой заряд и орудие без debug grant.
- **GUN-02:** transport/deploy не создают две машины и требуют допустимую позицию.
- **GUN-03:** отсутствующий crew/ammo/часть оборудования даёт конкретную остановку.
- **GUN-04:** load→aim→fire→recovery потребляет ровно один допустимый заряд/снаряд.
- **GUN-05:** отклонённая aim/selection не разрешает выстрел за рамками sector/policy.
- **GUN-06:** fast projectile swept collision не проходит сквозь тонкую стену.
- **GUN-07:** misfire/смена слота/reconnect не даёт reroll или бесплатного выстрела.
- **GUN-08:** игрок видит правильную машину/crew animation/дым/звук на всех adapters.
- **GUN-09:** capture/dismantle сохраняют компоненты и ownership, не дают целую пушку плюс полный refund.
- **GUN-10:** все 12 конструкций имеют проверенные различия назначения и исторический допуск.

### 15.8 Осады — SIEGE

- **SIEGE-01:** лестница/таран/метательная машина требуют реального подхода/цели/расхода.
- **SIEGE-02:** осадная башня достигает только допустимой стены и даёт реальный проход.
- **SIEGE-03:** блокада меняет подвоз контролируемых маршрутов, не магически все чужие склады.
- **SIEGE-04:** вылазка уничтожает/захватывает подходящую цель и меняет осаду.
- **SIEGE-05:** один незаметный клик колокола не захватывает население/казну.
- **SIEGE-06:** капитуляция меняет политические права, сохраняет жителей/семьи/личное имущество.
- **SIEGE-07:** артиллерийский пролом изменяет настоящие blocks/LOS/path/POIs и допускает проход пехоты.
- **SIEGE-08:** collateral destruction ограничен active war/protection, нельзя стрелять в чужой защищённый дом.
- **SIEGE-09:** offline window/объявление войны/reconnect не обходятся и не обнуляют уже принятый законный бой.
- **SIEGE-10:** ремонт/снос/loot после боя учтены один раз; восстановление снова делает здание функциональным.

### 15.9 Политика — POL

- **POL-01:** согласие общины/хартия дают управление по факту условий, не просто близость к site.
- **POL-02:** steward не отдаёт военный приказ без captain-права; captain не забирает казну без казначейского права.
- **POL-03:** NPC-совет исполняет бюджет/политику, не неограниченный расход.
- **POL-04:** вассальный договор передаёт существующий платёж/контингент; цикл сюзеренов отклоняется.
- **POL-05:** смена holder сохраняет один титул и актуальные обязательства.
- **POL-06:** наследование трёх законов даёт определённый результат, включая отсутствие/несовершеннолетнего наследника.
- **POL-07:** player death/respawn не дублирует унаследованное имущество и не запускает две succession operations.
- **POL-08:** война имеет цель, мир/перемирие исполняются с прежними людьми/землёй, не reset кампании.
- **POL-09:** пленные освобождаются/обмениваются по одному договору, не возвращаются в roster до физического перехода.
- **POL-10:** восстание использует существующих недовольных участников и выдаёт предупреждения/требования.
- **POL-11:** сведения посольства/карты ограничены правами и известностью, а не всем server state.
- **POL-12:** потерявший столицу дом может продолжить игру, договор/претензия не удалены автоматически.
- **POL-13:** подкуп переносит один реальный платёж в оговорённом результате и изменяет только допустимое решение; отказ/повтор/restart не дают бесплатного влияния или второй выплаты.
- **POL-14:** создание и раскрытие поддельной претензии меняют её доказанность/легитимность и отношения; документ сам по себе не передаёт титул/склад, повтор раскрытия не повторяет последствия.
- **POL-15:** поддержка претендента связывает деньги, существующих сторонников и контингент с одним политическим процессом; армия не появляется из воздуха, проигрыш не возвращает уже потраченные ресурсы.
- **POL-16:** саботаж снабжения требует физического доступа, законного для ruleset действия и учтённого повреждения/потери; нельзя повредить неизвестный чужой склад одной UI-командой, обойти protection или повторно получить награду за тот же effect.

### 15.10 Клиент и безопасность — CLIENT/SEC

- **CLIENT-01:** первый consent, отказ, изменённые permissions, неверный bundle/version и vanilla client — точные outcomes до Play.
- **CLIENT-02:** все экраны на 1280×720 и 1920×1080, нескольких GUI scales; строки читаемы, формы не обрезаны.
- **CLIENT-03:** preview соответствует blueprint/hash/rotation и не подтверждается при отсутствии модели.
- **CLIENT-04:** command-mode click не дублирует ordinary attack/use.
- **CLIENT-05:** Escape/chat/inventory/focus-loss/disconnect освобождают held actions.
- **CLIENT-06:** paging/selection остаются корректными при изменении roster и обновлении revisions.
- **CLIENT-07:** equipment/animation фазы следуют авторитетным state transitions.
- **CLIENT-08:** фильтры каталога/tech prerequisites/deny reasons понятны без debug-команд.
- **CLIENT-09:** полный content pack активируется на Fabric/NeoForge/Forge и освобождается при disconnect.
- **CLIENT-10:** старое событие соединения не удаляет pack/HUD нового сервера.
- **CLIENT-11:** скрытые map/entity/inventory данные не отправлены неавторизованному клиенту.
- **CLIENT-12:** полный каталог/LOD/sound load укладывается в измеренный budget и не ломает обычный Minecraft input.
- **SEC-01:** подмена actor/owner/price/quantity/target/revision не совершает effect.
- **SEC-02:** повтор context/action/operation не даёт вторую покупку/стройку/найм.
- **SEC-03:** отзыв роли/permission действует до принятия следующего effect со старого экрана.
- **SEC-04:** warehouse/cargo handles чужого owner не дают read/write; несуществующий endpoint не empty.
- **SEC-05:** malformed ZIP/path/size/hash/model reference fail-closed без partial activation.
- **SEC-06:** command flood не даёт частичной оплаты/неограниченной очереди; accepted operations доступны через status.

### 15.11 Crash/recovery — REC

- **REC-01:** crash между site discovery/claim/record сохраняет один adoption.
- **REC-02:** crash до/после reserve возвращает точный ресурсный баланс.
- **REC-03:** kill процесса внутри construction portion даёт blocks+consumption+receipt согласованно.
- **REC-04:** crash после craft output до plugin CAS не повторяет предмет/quality roll.
- **REC-05:** crash после gear transfer до hire finalization не даёт второй комплект/платёж.
- **REC-06:** group prepare/commit/apply при region migration сохраняет одно общее admission.
- **REC-07:** death/mount/demobilize не дублируют corpse loot/живого NPC.
- **REC-08:** succession/peace/tribute из durable intent завершаются без двойной передачи титула/денег.
- **REC-09:** crash между consumption/projectile/shot receipt даёт один выстрел либо ни одного по записанному решению.
- **REC-10:** crash при siege damage сохраняет совместимые blocks/salvage/POI состояние.

### 15.12 Симуляция и миграция — SIM/MIG

- **SIM-01:** dormant поле/шахта/стройка не создают продукцию при real-time ожидании/перезапуске.
- **SIM-02:** authorised active ticket позволяет реальную работу без владельца, в пределах admission budget.
- **SIM-03:** active→strategic→active сохраняет точные roster/HP/items/cargo.
- **SIM-04:** crash в каждом handoff шаге оставляет одну authority epoch/одну materialization.
- **SIM-05:** два одновременно прибывших клиента не получают две копии одной колонны.
- **SIM-06:** построенная после разведки стена блокирует arrival; груз не телепортируется сквозь неё.
- **SIM-07:** strategic conflict прекращается до открытия active physical сцены; нет фонового урона видимому игроку.
- **SIM-08:** reserve horizon/saturation дают явную паузу/ожидание без бесплатных запасов и бесконечных tickets.
- **MIG-01:** supported v1 snapshot проходит dry-run/migrate/verify с теми же identities/предметами.
- **MIG-02:** неподдержанный schema/worldgen identity отказывает без изменения исходного мира.
- **MIG-03:** content revision update сохраняет semantics уже начатых recipes/orders.
- **MIG-04:** detach без preparation запрещён; подготовленный вывод не удаляет неизвестные вещи молча.

### 15.13 Кампания — CAM

- **CAM-01:** fresh E1 campaign: найти деревню → экономика → найм → защита → первая местная практика, без debug.
- **CAM-02:** экономический игрок развивает город и покупает защиту без обязательного личного завоевания.
- **CAM-03:** капитан без собственного realm проходит контракт/снабжение/бой/оплату/продление.
- **CAM-04:** полная X–XV progression с фактическим производством позднего комплекта и батареи, достижимыми входами.
- **CAM-05:** война двух сторон, обоз, осада, мир, наследование, восстановление и restart в одном сохранении.
- **CAM-06:** varied-seed/biome multiplayer field testing плюс bounded adversarial exploration; owner failure не закрывается чужим более слабым сценарием.


### 15.14 Автономная кампания — дополнение к существующей приёмке

Все строки **required / not run / acceptance pending**. Prefix `ADD:` сохраняет ID внешнего owner addition и предотвращает коллизию с существующими ECO/WAR/SIM/REC IDs. Это уточнение тех же сценариев (связи справа), не второй независимый контракт. Product fixtures/tests/results принадлежат plugins, generic harness/API invariants — core, реальная presentation — Loader.

**Центральная вертикаль (A1–A5/R1–R3):** два поселения производят разные обязательные товары, первое резервирует груз/нанимает перевозчиков, второе escrow. Два игрока встречают ту же Party на дороге. В delivery варианте груз и платёж передаются один раз; в intercept варианте часть становится captor/loot, receiver фиксирует недопоставку и запрос охраны. Scout доставляет report, мобилизация уменьшает местный выпуск; восстановленная дорога/снабжение и демобилизация возвращают выживших к доступной работе. Каждый вариант включает logout/второго наблюдателя/restart без debug grants, фиксированный roster/stock и причины каждого результата.

| Addition ID | Действие → обязательный результат | Существующая связь / этап |
|---|---|---|
| ADD:INV-01 | Повтор hire/relocation/materialization → одна identity/assignment, representation не рождение | WAR-01, DEM-01/04, SIM-03; R1/R2 |
| ADD:INV-02 | Concurrent issue/withdraw последнего item → один outcome, без negative/double use | ECO-08, SEC-02; R1/R2 |
| ADD:INV-03 | Crash cargo debit до plugin ack → тот же operation, без второго груза | REC-02/04, ECO-12; R3 |
| ADD:INV-04 | Partial delivery затем cancel → только remaining reserve refunded, delivery/loss distinct | BUILD-03 semantics, ECO-12, REC-02; R3 |
| ADD:SIM-01 | Два игрока у remote group → один handoff/body/cargo set | SIM-04/05; ранний R1/R2 |
| ADD:SIM-02 | Наблюдатели у разных концов → реальные per-part позиции/состав, не всё у лидера | SIM-03/05, WAR-12; R2/R8 |
| ADD:SIM-03 | Activation oscillation → no HP/ammo/consumption/deadline/progress/RNG reset | ITEM-04, SIM-03; R2/R8 |
| ADD:SIM-04 | Crash каждой стадии handoff → один executor либо явный recoverable transition | SIM-04; R2/R8 |
| ADD:SIM-05 | Сломать bridge/поставить wall до arrival → stop/reroute из checked point | BUILD-08, SIM-06; R3 |
| ADD:SIM-06 | Parties crossing между callbacks → encounter до продвижения за контакт | SIM-07; ранний R2/R3 |
| ADD:SIM-07 | Logout с projectile → законный бой/effect продолжаются, не escape | SIEGE-09, WAR-14; R2/R4 |
| ADD:ECO-01 | Увести всех игроков от обоих поселений → admitted production/consumption реально продолжаются | ECO-09, SIM-02; R1/R3 |
| ADD:ECO-02 | Удалить station/закрыть doorway → blocked с причиной и без remote output | BUILD-05/07, ECO-06/10; CP-049 |
| ADD:ECO-03 | Intercept supply исчерпал еду → declared shortage/decisions; real food восстанавливает work | DEM-05, ECO-06/12; R1/R3 |
| ADD:ECO-04 | Мобилизовать половину workers → выпуск меняется, people/ration не удвоены | WAR-02/08; R2 |
| ADD:INFO-01 | Убить sole scout до delivery → recipient не знает report | POL-11, CLIENT-11; R2/R3 |
| ADD:INFO-02 | Убить scout после delivery → knowledge сохранено с observed_at | POL-11; R3 |
| ADD:INFO-03 | Получить foreign map payload → нет hidden exact stock/roster/coords | CLIENT-11, SEC-04; ранний R3 |
| ADD:WAR-01 | Смена alliance/owner во время order → current permission contract, stale order не обходит protection | WAR-14, SEC-03; R2/R5 |
| ADD:WAR-02 | NPC proxy вне PvP window → destructive action refused | SIEGE-08/09; до raid/R4 |
| ADD:WAR-03 | Road blockade + internal food/bypass → действительное supply продолжается | SIEGE-03, ECO-12; R4 |
| ADD:WAR-04 | Town hall capture при organized garrison → полное capture predicate, не один touch | SIEGE-05; R4 |
| ADD:WAR-05 | Lawful surrender/occupation → rights меняются, люди/private items/ruins сохраняются | SIEGE-06/10; R4/R5 |
| ADD:TIME-01 | Sleep/time set/downtime restart → no deadline acceleration/free result | TECH-07, SIM-01, §11; R1 |
| ADD:PERF-01 | Concurrent battles + autonomous economy → whole-tick SLO **и bounded service backlog** | PERF-A/C + activation/soak §12; R8 |
| ADD:REC-01 | Registry migration с pending hire/cargo → IDs/refs/intents проверены, no silent reset/orphan items | MIG-01/02, REC-05; перед localized cutover/R8 |

Acceptance также требует сопоставления physical/remote work сценария без выгоды переключения, ограниченного bootstrap/re-discovery, отказа replay старой policy для in-flight effects по единому договорённому контракту и цены каждой handoff стадии. Не пиновать названия/число modules как поведение.

## 16. Как получать доказательства

### 16.1 Canonical harness

Core canonical harness остаётся общим runner/engine tooling; settlement product scenes, acceptance fixtures/assertions и их ownership — plugins, Loader scenes — Loader. На конечном согласованном дереве integration owner запускает применимые gates; не mid-flight по half-edited siblings. Core correctness защищает generic invariants и не заменяет deployment + реальные plugin сценарии/клиент.

Реальный клиент — Minecraft 26.1.2 под Xvfb с утверждённым MCP/lifecycle events, Fabric/NeoForge/Forge. TCP/unit/mock не доказывают рендер/стройку/формации/выстрел. Не выдумывать `run settlements`, которого нет в canonical manifests.

`--check`/`--prepare` означают prepared, не pass. Timeout — failure. Событие producer будит consumer; никаких sleeps ради «стабилизировалось».

### 16.2 Evidence receipt

Для каждого outcome: source revisions/changed files, command/profile, seed/coords/config, player count, machine configuration, result.json path, status, logs, relevant screenshots/video/client state, измерения и точный незакрытый gap.

Исходные сценарии и evidence лежат у владельца соответствующего repository/harness. Mojang bytes, пользовательские credentials и raw секреты не переносятся в Git. Не публиковать токены MCP из environment.

### 16.3 Тестовая дисциплина

Постоянные тесты защищают реальные ошибки: conservation, duplicate operation, ownership, corruption recovery, timing/LOS, policy change, unload identity. Не тестировать совпадение Rust source text с желаемой строкой или что mocked argument просто прошёл через функцию.

Новые features сначала доказываются исполняемым smoke реального пути; regression оставляется для правдоподобной ошибки. Documentation-only работа, как настоящий комплект, требует static/link/number/DAG checks и независимого чтения, **не Cargo/игрового запуска**.

## 17. Основные риски и предписанный ответ

- **Контентный масштаб.** 768 вещей — отдельная большая авторская работа. Решение: category briefs, модель/поведение/источник admission, общий rig/atlas, ранний representative pipeline; не 768 временных заглушек.
- **Расхождение core/Loader.** Решение: R0 сквозной handshake и один UI до дальнейшего интерфейсного роста; documentation assertions не заменяют source check.
- **Подмена vanilla деревень.** Решение: разделить runtime catalogue и generator ownership; принимать существующий VillagePlan, не включать второй settlement generator.
- **Пассивная псевдоэкономика.** Решение: real work/reservation/haul/tickets + service deadline; постоянная remote starvation из-за ресурсов — непринятый масштаб, не нормальный dormant.
- **WASM/host limits и registry монолит.** Решение: локальные domain records/indexes, bounded due/dirty projections и native induced-work admission; fuel не лечит blocking host call.
- **Сетевое снаряжение.** Решение: полный lifecycle custom identity; carrier/model недостаточно для damage/save semantics.
- **Бой/pathfinding.** Решение: measured physical encounter/formation/perception; aggregate только после predeclared gate, без per-frame guest microcommands и idle-scale обещаний.
- **Offline война/фон.** Решение: war windows, authority handoff и reserve horizons до strategic enablement; не «скрытая магия offline».
- **Смешение средневековья с vanilla.** Решение: явно описанные adapter rules без глобального удаления Minecraft progression; баланс проверять с игроком, умеющим строить/копать.
- **Спекулятивная сложность политики.** Решение: конечный именованный набор laws/treaties/events, один title authority; не копировать все подсистемы CK3.

## 18. Решения, которые уже можно брать в работу

Зафиксированы как рекомендуемый целевой дизайн: развитие X–XV с локальной практикой, обязательный Loader, vanilla village adoption, 768-план/650-minimum, четыре armor slots с составными профилями, physical emerald economy, individual residents, физическая осада, server-authoritative combat и bounded background transit.

Перед implementation отдельного этапа следует согласовывать только материальные изменения результата: сдвиг исторических границ, отказ от части семейств/минимума 650, изменение offline-PvP правил, смена валютной authority, существенно иной целевой масштаб. Не спрашивать заново о каждом очевидном DTO/имени файла и не объявлять эти рекомендованные defaults уже одобренными владельцем.

Ближайшая **приёмка** — generic purge/API и CP-049 actual BUILD-05/07 на deployment/native work. Реализация остальных CP-050–096/A1–A8 разрешена параллельно прямым решением владельца, закрытие только по зависимостям и evidence в §1.5. R0 Loader-required vanilla adoption остаётся первым полным client outcome и pending; существующие P/CP evidence сохранены. Ruleset defaults (finite admission, no downtime catch-up, emerald/barter, tickets, physical encounters first, courier knowledge, House/dynasty) остаются проектными, не новой утверждённой production balance. Numerical windows/rations/speeds/aggregate tolerances фиксируются перед соответствующим gate; ownership/physical effects/recovery/permissions от баланса не зависят.
