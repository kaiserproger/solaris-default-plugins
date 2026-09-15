# Solaris Settlements — техническая спецификация реализации

Статус: **draft · проект**, 2026-09-15. Не claim о готовности runtime.
Игровые требования: [SPECIFICATION.md](SPECIFICATION.md). Контент: [CONTENT_SPEC.md](CONTENT_SPEC.md).

## 1. Проверенная исходная точка

Изучены рабочие файлы, а не только Git HEAD. На момент чтения:

- `solaris-default-plugins`: `1d7db97266ea0d69ce0b530a5e4253977db20932`;
- `solaris`: `78b4f39d6a9363948c46f4983b243c9ce3fc97c9`;
- `solaris-loader`: `3aaa92663ce8bc3e7de2859ad40ed357aacf3382`.

Это идентификаторы основы исследования; чужие незакоммиченные изменения не сбрасывать. Для implementation-checkpoint нужно зафиксировать собственный bounded working-tree snapshot. В этой работе production-код не меняется и игровые сценарии не запускались.

### 1.1 Пакет уже не пустая заготовка

[plugin.toml](../../solaris-settlements/plugin.toml) объявляет `solaris-settlements`, version 1.0.0, API 0.6.0, storage/storage_batches, inventory_transfers, persistent_residents, resident_work, resident_orders, world_sites, structure_operations и player_queries. `[client]` и worldgen selector отсутствуют.

[main.lua](../../solaris-settlements/main.lua) — около 6 тысяч строк: записи поселений/зданий/жителей/отрядов, durable intents, CAS, найм/демобилизация, снабжение и диспетчер команд. Текст прямо фиксирует server-only состояние. Текущая циклическая логика проверяет присутствие игроков/актуальность supply projection; это не готовая автономная симуляция государства.

[README.md](../../solaris-settlements/README.md) описывает 22 авторских чертежа и команды создания, принятия site, survey, строительства, профессий, найма, squad orders и ролей. Чертежи/генератор сохранять и развивать, а не заменять с нуля.

[config.toml](../../solaris-settlements/config.toml): 24 поселения, 64 здания, 40 жителей, 8 squads, 16 pending operations; цикл 1200 ticks. Это текущие ограничения прототипа, не масштаб нового overhaul.

### 1.2 Где документация расходится с кодом/другими документами

- README/main.lua пакета ещё называют writable warehouse и catalogue wiring отсутствующими. Текущий [core PLUGINS.md](../../../solaris/docs/PLUGINS.md) уже документирует `bind_warehouse`, player↔warehouse transfer и включение runtime-каталога через `structures/` + required features. Старое сообщение не является надёжной полной картой нынешнего ядра.
- При этом документированный warehouse transfer допускает **одного игрока и один склад**; resident↔warehouse и warehouse↔warehouse этим путём отказываются. Полную логистику нельзя считать готовой из-за наличия `warehouse` в union.
- Core имеет [client_view.rs](../../../solaris/crates/mc-script/src/client_view.rs), а [lua.rs](../../../solaris/crates/mc-script/src/lua.rs) задаёт `CLIENT_MANIFEST_SCHEMA = 2`. Core-документация описывает wire 3/schema 2 views.
- В текущем Loader [LoaderHandshake.java](../../../solaris-loader/loader-core/src/main/java/dev/solaris/loader/LoaderHandshake.java) задаёт `PROTOCOL_VERSION = 2`; [LoaderContentArchive.java](../../../solaris-loader/loader-core/src/main/java/dev/solaris/loader/LoaderContentArchive.java) задаёт `INDEX_SCHEMA = 1`, 128 items, 128 assets, 64 KiB index, 64 MiB activated assets, 1 block per bundle/8 blocks aggregate. Полный сквозной новый UI не подтверждён.
- `docs/MEMORY.md` в core наверху ещё говорит «vanilla villages не генерируются». Текущие [main.rs](../../../solaris/crates/mc-server/src/main.rs), `village_plan_source_for_startup`, и [VILLAGE_GENERATION.md](../../../solaris/docs/VILLAGE_GENERATION.md) уже содержат включённый vanilla village path. Этот код имеет явное отключение core-villages при наличии plugin settlement plan.
- Core документирует terrain-adaptation как column-height analogue, а не полную ванильную density arithmetic; не все template mobs спавнятся. Не заявлять полную vanilla parity только на основании слова `vanilla`.

Вывод: есть полезный работающий каркас и отдельные upstream-слои, но **нет проверенной единой client+server поставки такого overhaul**. Предреализационный контракт нужно замыкать по фактическим источникам и реальным сценариям, а не «повторно написать всё C1–C4» или довериться устаревшим `MISSING_CORE_CALLS`.

### 1.3 Какие существующие решения использовать

Постоянные resident handles, intent/result/operation_status, CAS и storage_scan, bounded inventories/reservations, site survey, staged construction, work/order DTO, декларативные клиентские bundles и точную проверку SHA-256. Эти механизмы — основа, не конкурирующие версии новой архитектуры.

Не копировать `solaris-towns` в settlements: towns управляет игроками/землёй, а здесь NPC-общество. Не выдумывать cross-plugin money service. Не хранить фактическое содержимое склада одновременно в plugin ledger и chest.

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
- canonical harness.

В core не живут список «восемь культур», цены оружия, законы наследования, баланс налога или enum конкретных settlements-мечей. Общий native `equipment profile` допустим; `if plugin == solaris-settlements` для механики недопустим.

### 2.2 Luau-пакет — игровая политика

В `solaris-default-plugins/solaris-settlements`:

- принятие/развитие поселений и права участников;
- семьи, династии, титулы, договоры, технологии;
- хозяйственные планы, заказы, kit/doctrine, обучение и найм;
- зарплаты/рента/торговые контракты, выбор реальных источников;
- определение врагов/союзников через bounded policy;
- очереди строительства и интерфейсные модели;
- авторские content data, blueprints, assets и localization.

Политика вызывает общие исполняемые механики; не loop `damage_entity` для боя, не 500 `move_to` каждый tick, не генерация блоков из Lua на worldgen worker.

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

Production-манифест переключается на Loader-required только после согласованного core/Loader пути; не делать несовместимый обязательный клиент преждевременным default. После cutover текущий server-only путь не поддерживается параллельно как скрытый fallback полной игры.

### 3.2 Предлагаемая структура авторских исходников

```text
solaris-settlements/
  plugin.toml
  config.toml
  main.lua
  modules/
    settlements.lua
    population.lua
    economy.lua
    logistics.lua
    technology.lua
    military.lua
    diplomacy.lua
    dynasties.lua
    views.lua
    recovery.lua
  content/
    items/
    recipes/
    technologies/
    jobs/
    unit_kits/
    engines/
    campaigns/
  structures/
  client/
    views/
    models/
    textures/
    animations/
    sounds/
    lang/
```

Это целевые пути, сейчас не объявленные поддерживаемыми package artifacts. До их применения расширить strict discovery и authoring contract. Не добавлять произвольные файлы с надеждой, что loader их проигнорирует.

Для Luau нужен минимальный **startup-only owner-relative module mechanism**: перечисленные/проверенные файлы, один VM, кэш результата модуля, запрет циклов/выхода из пакета, strict type-check каждого файла, общий memory/fuel budget. Это предлагаемая host-возможность, не утверждение о работающем `require`. Она нужна для уже большого main.lua и конкретных доменных границ, а не универсальная система динамических плагинов.

Контент читается/компилируется native startup loader вне 16 MiB Lua heap; VM получает компактные IDs/разрешённые запросы, не копию всех моделей и 768 больших таблиц.

### 3.3 Версии

Различать server API version, package version, bundle schema, transport wire, content revision, save schema и worldgen identity. Номер `0.6.0` не означает автоматическую поддержку всего нового.

Не назначать новый wire/schema номер только из этой спецификации: сначала сверить существующие ветки core/Loader, затем один согласованный version cutover во всех validators/adapters/fixtures. Неподдерживаемый feature отклоняется до Play; нет старого decoder параллельно «на всякий случай».

## 4. Принятие деревень и здания

### 4.1 Core bridge

Существующий `VillagePlanSource` остаётся источником генерации. Нужен публичный bounded site descriptor для фактически сгенерированной деревни: stable site id, dimension, layout revision, bounds, materialized pieces, known POIs, inhabitant identities и provenance.

**Два разных действия:** core сгенерировал деревню; plugin принял её в своё управление. Принятие не должно требовать plugin worldgen plan, который выключает core villages. Catalog owner runtime-зданий и генерационный selector разъединяются при необходимом cutover, без второго генератора.

Site enumeration не генерирует бесконечный мир. Она возвращает уже известные/зарегистрированные сайты в разрешённой области/странице. До знакомства игрока нейтральные деревни могут существовать без полного Lua roster.

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

У существующего `claim_resident` использовать текущую семантику; не добавлять второе API «преврати ближайшего жителя». Незавершённое принятие имеет intent per resident/site и восстанавливается. Частично принятая деревня не объявляется fully operational, пока конфликтующие owners/POIs не разрешены.

Идентичность жителей генератора не пересчитывается из их текущей позиции. Игрок может увести жителя; он остаётся тем же человеком, а не новой местной демографической единицей.

### 4.3 Здания игрока

Новый bounded building certificate использует существующие survey/POI проверки. Заявка содержит ограниченный footprint, purpose, proposed interaction points и expected world revision. Сервер проверяет реальную доступность/станции/контейнеры. Повторное распознавание одного объёма не создаёт дополнительные capacity/workplaces.

Construction blueprint и certificate — разные способы получить одинаковые native functional predicates. Никакого второго «производства только в custom домах».

## 5. Модель данных и единственная authority

### 5.1 Идентификаторы

`world_id`, `site_id`, `settlement_id`, `resident_handle`, `family_id`, `dynasty_id`, `realm_id`, `title_id`, `squad_id`, `army_id`, `engine_id`, `operation_id`, `content_id` — opaque strings или typed internal keys. Координаты не identity; connection-local numeric id не пишется в save как личность.

Числа Luau только точные safe integers `0..2^53-1`; крупные counters/UUID/hash сериализовать как строки. Не передавать u64 в double и надеяться на CAS.

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
- `Squad`: roster refs, commander, kit/doctrine, active group order, morale projection, supply contract.
- `Army`: ordered squad refs, command hierarchy, supply plan, route, campaign objective.
- `ServiceContract`: exact people/company, term, equipment provenance, wage/ration obligation, escrow refs.
- `EngineAssignment`: native engine handle, crew handles, commander, ammunition source, order refs; без второго ammo/HP.
- `Dynasty`: bounded living/important members, lineage edges, succession policy.
- `Title`: holder, realm, territory refs, inheritance law, claims.
- `Treaty`: parties, obligation definitions, start/end tick, signatures, state.
- `War`: parties, goal, lawful engagement/destruction policy, siege windows, occupation refs.
- `OperationIntent`: kind, stable ids of child operations, fingerprint, reservations, last committed receipt, recovery state.

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

## 6. Экономические и inventory API

### 6.1 Сохранить существующую семантику

Использовать `query_owned_inventory`, `transfer_owned_items`, `reserve_inventory_items`, `inventory_reservation_status`, `release_inventory_reservation`, `storage_batch_cas`, `storage_scan`, `operation_status` и их actual DTO. Примеры ниже описывают недостающую возможность, не переименование экспортированных функций.

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

Новый native immutable recipe registry с закрытыми ingredient/component predicates и workstation capabilities. Он расширяет настоящий crafting/work путь, не создает Lua-side «результат появился».

Новые outcome fields: committed portion id, exact inputs consumed, exact outputs/materialized intermediate, wear deltas, resulting endpoint fences, pause reason, recipe revision. Handler получает только bounded агрегат/страницу, а не полную историю мастерской.

### 6.4 Изменения состояния как события

Требуются owner-scoped notifications для inventory/reservation/work/order/combat/lifecycle/site/policy. В текущей документации work/order/combat notifications ещё отмечены как отсутствующие — полный overhaul не должен строиться на постоянном опросе всех NPC.

Event содержит producer sequence, identity, revision и typed delta. Событие — сигнал перечитать/применить проекцию, не отдельная экономическая authority. Потеря события восстанавливается snapshot с cursor; повтор не повторяет игровой effect.

## 7. Сохранение, транзакции и recovery

### 7.1 Durable operation

Одна операция получает стабильный operation id до эффекта. Повтор с тем же canonical fingerprint возвращает прежний результат; иной payload с тем же id — conflict. Request id коррелирует доставку, не дедуплицирует оплату после reconnect.

Важные состояния: rejected, accepted, running, paused, committed, cancelled. После возможного durable append нельзя объявлять результат отвергнутым только по timeout. UI показывает pending/recovering и запрашивает тот же id.

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

Нужен измеренный **generic campaign storage capacity profile**, не отключение всех лимитов:

- проектная admission цель: 65 536 live records per campaign owner;
- существующий небольшой value bound по возможности сохраняется; длинные roster/history шардируются;
- native serialized storage budget: предварительно до 256 MiB, отдельно от Luau heap;
- открытые операции/резервы получают зарезервированную capacity;
- неактивные поселения не загружают весь social graph в 16 MiB VM;
- список/scan постраничный, mutation batch остаётся bounded; большая succession использует recoverable process, а не невозможный CAS всех тысяч записей.

Это proposed profile, который должен пройти нагрузку/восстановление. Простое увеличение `maximum_residents` в config не решает host limits.

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

Общий damage resolver учитывает server PvP/protection/affiliation и принятую doctrine. Luau получает committed combat event для опыта/политики. Число целей одного действия и max collision sweep bound фиксируются профилем, не произвольным полем пакета.

### 8.3 NPC perception и command policy

Local spatial index, ограниченные candidate sets, LOS по настоящему миру, current policy revision. Старый target ref перестаёт действовать при expiry/смерти/смене союза/невидимости согласно виду приказа. Разведчик не превращает все сведения сервера в карту.

NPC реагируют на сигналы producer: изменение цели/пути, прибытие supplies, разрушение POI, приказ, атака. Не сканировать все поселения/отряды every tick в Lua.

### 8.4 Squad admission и армия

Существующее group admission ≤64 handles сохраняет проверку каждого fence до общего решения. На уровне армии plugin раскладывает intent в bounded squad operations; UI показывает принимаемые/отказанные части, а не выдумывает глобальную атомарность.

При совпадении нескольких командиров действуют delegation ACL и expected order revision. Policy change обновляет право атаковать независимо от медленной доставки нового UI. Dead/unloaded/migrating member имеет конкретный per-member outcome.

### 8.5 Mounted actors

Райдер и mount — две entities и один проверяемый binding. Native pathing использует размеры/движение mount. Mount death/dismount/region transfer не создаёт повторного всадника и не оставляет невидимый carrier. Экипировка коня имеет свой endpoint и provenance.

### 8.6 Composite machines

Закрытые engine definitions: collider shapes, hit zones, rig, sockets, crew work slots, inventory endpoints, allowed motion, deploy anchor, fire profile, dismantle outputs. Не универсальный scripting движущихся чанков.

Состояния машины и отдельного crew job согласованы: отсутствие наводчика может запретить aim, а отсутствие одного заряжающего замедлить допустимый процесс только по определению. Crew checks перед commit; смерть расчёта после shot commit не отменяет уже летящий снаряд.

### 8.7 Разрушение

Damage по зданию адресует bounded volume/section и canonical blocks. Проверить права войны и обычную защиту до commit. Структурный профиль определяет сопротивление и локальные группы collapse; не сканировать опоры всего мира.

В одном recoverable portion: changed blocks, salvage/drop, invalidate POI/path/collision, damage receipt. Клиентский debris purely visual, не второй источник материалов. Обычная кирка и снаряд идут через согласованный world mutation/invalidation, иначе получится невидимый целый дом с дырой в модели.

## 9. Фоновая симуляция без подмены мира

### 9.1 Три режима

- **Active physical:** chunk/region загружены; canonical entities/blocks, настоящее движение/работа/бой.
- **Strategic transit:** только допущенные уже существующие люди/грузы на заранее проверенном маршруте; один native transit owner, без активных entity copies.
- **Dormant settlement:** социальные записи сохранены, физическое производство и стройка остановлены; UI показывает причину. Не добывать шахту/дерево «расчётом дохода».

Город может получить bounded simulation ticket, чтобы оставаться физически активным без online владельца. Такие tickets — ограниченный measured resource; они не бесконечные chunk loaders, выдаваемые каждому жителю.

### 9.2 Что разрешено стратегическому режиму

Путь/израсходованный рацион, сроки договоров, старение и политические уведомления от campaign clock; столкновение двух transit parties на известном пустом участке, если оно не касается active world. Для dormant поселения потребности/сроки учитываются до безопасного горизонта имеющихся резервов; после него переход требует waking physical simulation либо явной паузы. Нельзя бесплатно заморозить захваченную кампанию, но нельзя и придумать съеденный невидимый урожай.

Background conflict использует те же authoritative roster, gear и resource inputs; алгоритм агрегирует обмен по детерминированным seed/порциям, а не выбирает победителя из одного total power. Его статистическое соответствие physical resolver проверяется отдельно; он не доказательство поведения видимой битвы.

### 9.3 Ключевой handoff

`active → parking → strategic → materializing → active` с exclusive authority epoch. Перед parking: сохранить точные HP/equipment/cargo/позиции/приказы и убрать physical executors после commit. Перед materializing: остановить стратегическое продвижение, выбрать достигнутую позицию на проверенном маршруте, проверить занятость/terrain и зарезервировать места.

Если место изменено, маршрут blocked: не телепортировать за стену и не терять груз. Выбрать последнюю подтверждённую доступную точку либо остановить вход в область по явному admission result. Появление игрока отменяет дальнейшие strategic portions на затронутом участке **до** открытия ему физической сцены.

Crash на handoff: replay epoch даёт ровно одно представление. Два игрока с разных сторон границы не создают два materialization request с двумя roster-копиями.

### 9.4 Какие операции запрещены в фоне

Break/place arbitrary block, harvest реального поля, craft без доступного подтверждённого station/reserve executor, разрушение стены, PvP-попадание, изменение частного контейнера в active world. Артиллерийская осада всегда физическая в затрагиваемом участке.

Эта граница сознательно ограничивает бесплатную offline-экономику, но не конечную стратегическую карту/дальние походы. Если tickets/loaded region отсутствуют, queued construction ждёт; это не недоделанный «успех через таймер».

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

Для повторяющихся расчётов сохранять last_applied_tick/sequence и рассчитывать bounded next slice. Большой долг времени не исполнять одним Lua handler, превышающим fuel; scheduling продолжает работу с сохранённого cursor. Изменение климатического сезона уведомляет затронутые участки, не сканирует весь мир в каждом tick.

## 12. Производительность: цели, не обещания

### 12.1 Почему текущих defaults недостаточно

24×256 гражданских = 6144 человек; плюс 2048 военнослужащих = 8192 roster entries. Дополнительно семьи, здания, склады, операции, задания, договоры и материалы. Это уже больше старой заявленной квоты storage records; полный state нельзя постоянно держать в каждой Lua таблице.

В бою сложность определяется не только числом NPC: path replans, formation collision, projectiles, spectators и world destruction. Нельзя вывести 512-man battle из успешного теста 512 idle entities.

### 12.2 Нагрузочные сценарии

**PERF-A:** 80 гражданских +64 военных, два игрока, второй загруженный населённый пункт, одновременно haul/craft/build/combat; debug-прогоны доказывают поведение.

**PERF-B:** 512 военнослужащих суммарно, 8 орудий, 4 клиента, смешанные melee/ranged/cavalry, перемещение через два региона, разрушение секции стены, обоз. Один бой, полный реальный сценарий.

**PERF-C:** 24 поселения/6144 гражданских +2048 военнослужащих в roster, 32 сессии, минимум два одновременно активных независимых боя по 256 бойцов; остальные территории в разрешённых режимах, длительная экономика/сохранение.

**PERF-D:** весь фактически принятый каталог (≥650 personal military items) плюс обязательные материалы/машины в activated registry; список/поиск/отображение всех production-моделей, смешанные комплекты, packet bytes, heap/native/GPU consumption. Отдельная capacity-нагрузка на 768 definitions может использовать явно помеченные stress fixtures; она не требует 768 принятых production-карточек и не засчитывает fixtures в контентную квоту.

Производительность окончательно мерить explicit performance/release профилем; обычная разработка debug. Для целевых 20 TPS на зафиксированном reference host: server tick p95≤50 ms, p99≤60 ms на steady-state участке; latency приказа admission→наблюдаемая реакция p95≤250 ms без искусственной сети. Превышения/паузы считать failure, а не скрывать средним.

Это **проектные пороги**, не утверждение о достижимости на каждом клиенте/сервере. Reference CPU/RAM/GPU, seed, distance, сборка, число viewers и run duration обязательны в отчёте. Нельзя молча снизить NPC count, если сценарий не проходит.

### 12.3 Допустимые оптимизации

- immutable catalog once at startup, shared IDs без копирования definitions;
- regional ownership и spatial indexes;
- стабильные formation slots и общий маршрут отряда с локальным обходом;
- path requests по invalidation/goal changes, не each tick;
- nearest visibility subscriptions, delta replication, LOD rendering;
- native bounded work batch и события вместо Lua per-entity ticks;
- ограниченные active tickets и сохранённая стратегическая identity.

Не начинать со смены ECS/распараллеливания всего сервера без измеренного bottleneck. Всякая оптимизация должна сохранить common-play behavior, указанную границу и evidence before/after.

### 12.4 Saturation

До создания поселения/найма/размещения проекта резервируются roster/storage/active job capacities. Full queue даёт explicit pending/rejected result и не частичную оплату. Существующие активные операции имеют возможность завершить commit/recovery; новый admission не вытесняет их в потерю предметов.

На клиенте снижение визуальной детализации не меняет хитбоксы/LOS. На сервере нельзя пропустить урон по кадру, потому что рендерный LOD выключил модель.

## 13. Миграция и отключение

### 13.1 Новый мир — основной проверяемый путь

Новая campaign запускается на vanilla settlement generation с отдельным gameplay overlay. Генерационные параметры не изменяются только ради принятия village. На старте выбранные content/campaign versions фиксируются вместе с save.

### 13.2 Существующий server-only v1

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

До первого разрушительного действия R4 реализует минимальную общую `War` authority: стороны, объявление, допустимая цель, protection predicates, осадное окно и grace period. Это prerequisite внутри этапа, а не обход до готовности политики. R5 расширяет те же записи договорами и политическими основаниями, не создаёт второй источник разрешения войны.

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

Результат: один authoritative physical/strategic handoff, многопоселенческая кампания целевого roster, минимум два активных боя, длительный save/restart, supported v1 migration, detach/update, все Loader platforms.

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

## 16. Как получать доказательства

### 16.1 Canonical harness

Из core workspace использовать существующий `python3 -m tools.harness`. Для Rust code commit — один финальный `run correctness`. Для Java/Loader — `run java` и `run loader-live --platform fabric|neoforge|forge` на каждой платформе с необходимым реализованным scene manifest. Custom settlements scenarios добавляются в соответствующие canonical manifests/backends, не в произвольный bash loop.

Реальный клиент — Minecraft 26.1.2 под Xvfb с утверждённым MCP и lifecycle events. TCP/unit/Luau mocks могут защищать инварианты, но не доказывают рендер, стройку, formation или cannon fire.

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
- **Пассивная псевдоэкономика.** Решение: real work/reservation/haul; dormant pauses явно видны; benchmark не вознаграждает скрытую генерацию денег.
- **Lua монолит и host limits.** Решение: owner-relative checked modules, native catalogs, bounded projections и capacity admission; не увеличивать handler fuel на порядок без причины.
- **Сетевое снаряжение.** Решение: полный lifecycle custom identity; carrier/model недостаточно для damage/save semantics.
- **Бой и pathfinding.** Решение: измеренные native formation/perception workloads; не microcommands из Luau и не обещания idle-scale.
- **Offline война/фон.** Решение: war windows, authority handoff и reserve horizons до strategic enablement; не «скрытая магия offline».
- **Смешение средневековья с vanilla.** Решение: явно описанные adapter rules без глобального удаления Minecraft progression; баланс проверять с игроком, умеющим строить/копать.
- **Спекулятивная сложность политики.** Решение: конечный именованный набор laws/treaties/events, один title authority; не копировать все подсистемы CK3.

## 18. Решения, которые уже можно брать в работу

Зафиксированы как рекомендуемый целевой дизайн: развитие X–XV с локальной практикой, обязательный Loader, vanilla village adoption, 768-план/650-minimum, четыре armor slots с составными профилями, physical emerald economy, individual residents, физическая осада, server-authoritative combat и bounded background transit.

Перед implementation отдельного этапа следует согласовывать только материальные изменения результата: сдвиг исторических границ, отказ от части семейств/минимума 650, изменение offline-PvP правил, смена валютной authority, существенно иной целевой масштаб. Не спрашивать заново о каждом очевидном DTO/имени файла и не объявлять эти рекомендованные defaults уже одобренными владельцем.

Ближайший законченный implementation outcome — **R0: один совместимый Loader-required пакет принимает реальную ванильную деревню, сохраняет её людей/блоки и показывает правдивый обзор/склад на трёх adapters**. Это первый этап большого overhaul, не его финальная поставка.
