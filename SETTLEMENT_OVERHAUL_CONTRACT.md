# Solaris Settlements: upstream-контракт overhaul

Статус: **предложение для реализации, не описание работающих возможностей**.
Дата: 2026-09-09. Текущий результат задачи — этот контракт; изменения ядра,
Loader и игровых пакетов ещё не выполнены.

Подтверждено владельцем: полноценное объединение поселений и найма жителей;
развитие деревни в манор, замок или город; ориентиры Bannerlord / Total War /
Manor Lords; **обязательный Solaris Loader**. После обнаружения ограничений
API владелец выбрал сначала полный upstream-контракт, а не урезанное
объединение существующих прототипов.

Численные параметры ниже — предлагаемые стартовые настройки и критерии
приёмки, а не уже измеренная производительность или одобренный баланс.
Все перечисленные новые API, поля и пути артефактов — предложения. Их нельзя
добавлять в production-манифест до появления реализации и strict-проверки.

## 1. Источники и проверенная исходная точка

Локальные HEAD при исследовании:

- plugins: `d71ce84d407244baec26cfd1c6a9c8c4392d19c4`;
- core: `6a383c2d38e271dff16a3f4e2350010b7ebbe03f`;
- Loader: `ca51a34346c4acbe33391cb2616dfd6ee1725b56`.

Исследовалось содержимое рабочих деревьев, не только HEAD. В core уже есть
чужие незакоммиченные изменения, включая `mc-server/src/main.rs`,
`mc-net/src/server.rs` и worldgen; контракт не даёт разрешения их перезаписывать.

Референсы:

- [Тред Minecraft](https://www.reddit.com/r/Minecraft/comments/1w82y7z/what_would_a_updated_village_look_like/).
  Сетевое чтение обсуждения не удалось; прочитаны все 14 страниц предоставленного
  PDF `What Would a Updated Village look like_ _ r_Minecraft.pdf`.
- Страницы 2–4: хутор на 4–5 домов, деревня на 9–12, город на 20+;
  специализации по местности, редкость крупных поселений, восстановление руин.
- Страницы 5–7 и 10: плотная планировка, пригодный рельеф, доступные двери,
  связанные дороги, оборона и зависимость размера от свободной земли.
- Страница 11: строители и постепенная перестройка домов.
- [Guard Villagers](https://github.com/seymourimadeit/guardvillagers):
  местная стража, оружие/броня, защита жителей, пост/сопровождение, снабжение.
- [Villager Recruits](https://github.com/talhanation/recruits): найм,
  группы, приказы и интерфейсы армии. README указывает All Rights Reserved;
  код, изображения, модели и звуки не копировать. Нужна собственная реализация.

Числа плотности «городского биома» из комментариев не принимаются как формула
генерации: редкость задаётся проверяемыми весами кандидатов, а не обещанием
конкретного числа городов на произвольной площади.

| Сейчас | Доказательство | Чего это не даёт |
| --- | --- | --- |
| Найм, роли, home/hold/follow; до 8 активных членов на игрока в конфиге плагина | [colony main](colony-villager-scaffold/main.lua), [config](colony-villager-scaffold/config.toml) | Постоянной личности нанятого бойца и боевого поведения ролей |
| Один профиль, три уникальных шаблона, до 16 жителей | [manifest](settlement-prototype/plugin.toml), [Lua manifest](../solaris/crates/mc-script/src/lua.rs) | Разных размеров, произвольного каталога зданий и runtime-роста |
| Привязка к ближайшему жителю, временный токен, move/idle/release | [adapter](../solaris/crates/mc-net/src/script/villager.rs) | Восстановления того же бойца после истечения токена/рестарта |
| set_block задаёт default state одного блока | [API documentation](../solaris/docs/PLUGINS.md) | Поворота лестниц, парных дверей/кроватей, безопасной стройки |
| Storage CAS и inventory/storage commit | [API documentation](../solaris/docs/PLUGINS.md) | Crash-атомарности между журналом storage и playerdata |
| Loader screen/hud, текст, статические кнопки и клавиши | [UI definition](../solaris-loader/loader-core/src/main/java/dev/solaris/loader/LoaderUiDefinition.java), [screen](../solaris-loader/loader-platform-common/src/main/java/dev/solaris/loader/minecraft/LoaderTextScreen.java) | Динамических списков отрядов, форм, выбора на местности |

`solaris-towns` управляет объединениями **игроков** и земельными участками,
а не NPC-поселениями. Он не поглощается и не удаляется автоматически.

## 2. Владение и итоговая поставка

Итоговый пакет: `solaris-settlements`, `api = "0.6.0"`, один владелец
поселений, рекрутов, доменных записей и клиентского bundle. Новые bounded API
расширяют текущий контракт; произвольный доступ Luau к миру/ECS запрещён.

| Репозиторий | Ответственность |
| --- | --- |
| `solaris-default-plugins` | Правила поселения, уровни/размеры/специализации, цены/снабжение, очередь строительства, состав отрядов, дипломатическая классификация целей, авторские чертежи, клиентские декларации |
| `../solaris` | Проверка манифеста, terrain/world snapshots, размещение и восстановление операций, постоянные owner-scoped сущности, физическое выполнение работ/боёв, инвентари, доставка DTO, canonical harness и core-документация |
| `../solaris-loader` | Закрытая схема экранов/интеракций, проверка bundle, формы/списки, world selection/preview, клавиши, отображение; никаких авторитетных денег, урона или строительства |

Core MUST собираться и запускаться без обоих sibling-репозиториев.
Интеграционные сценарии явно получают пути к пакетам/Loader. Не возвращать
compiled-in first-party packages. Межплагинных сервисных контрактов нет.

В deployed set остаётся максимум один settlement profile; geological ore
profile может иметь другого владельца. Нельзя устанавливать одновременно
новый пакет и старый `settlement-prototype`.

## 3. Игровая модель

### 3.1 Независимые оси развития

- `size_class`: small / medium / large — вместимость выделенной пригодной
  территории, а не текущий prestige level.
- `stage`: hamlet / village / developed — общий путь развития.
- `branch`: none / estate / fortress / town — специализация развитого поселения.
- `branch_level`: 0–2; ветки: поместье → манор лорда; укреплённое поселение →
  замок; торговый посад → город.
- `specializations`: farming / forestry / ranching / fishing / mining;
  максимум две, только при наличии подходящих ресурсов и рабочих мест.
- `condition`: inhabited / partially_ruined; руины не считаются готовыми
  мастерскими, жильём или действующей обороной.

Малое поселение может стать компактным манором, но не получить фиктивную
вместимость большого города. Расширение требует отдельного обследования
соседней территории. Ветки не сносят окружающую деревню; замок/манор/городской
центр достраиваются как район. Смена ветки после первого строительства —
переустройство с новой сметой и проверкой места, а не бесплатный сброс.

| Стартовый тип | Жилые дома | Жители при заселении | Генерация |
| --- | --- | --- | --- |
| Hamlet | 4–5 | 8–16 | 65% принятых кандидатов |
| Village | 9–12 | 24–40 | 30% принятых кандидатов |
| Town | 20–28, часть двухэтажных | 50–80 | 5% принятых кандидатов |

Это веса типов, не вероятность на каждый чанк. Если большой вариант не
помещается, кандидат отклоняется; не превращать все неудачные города в хутора
и не выдавать статистику до terrain-фильтра за фактическое распределение.
Стартовые города могут включать разрушенные стены/донжон. Полностью
восстановленный замок, манор или развитый город достигается игрой.

### 3.2 Архитектура и рост

Базовый авторский стиль по изображению: каменный цоколь, деревянный каркас,
скатные крыши, рынок с навесами, улицы вокруг площади, огороды и пастбища
снаружи жилого ядра, крупный зал/храм как ориентир. Не копировать неизвестную
карту из изображения и не обещать её точное воспроизведение.

Каталог обязан покрывать: дома двух размеров, колодец/площадь, рынок,
склад/амбар, ферму, лесопилку, загон, рыбацкую пристань, шахтный вход,
кузницу, казарму, караульный пост, частокол/ворота, каменные стены/башни,
манор/зал, донжон, ратушу и библиотеку/общественное здание. Каждая постройка
имеет физические входы, POI, коллизии и работающие соответствующие функции.

Схема роста: обследование → проект → резерв материалов → стройка по этапам
→ проверка завершения → заселение/рабочие места → пересчёт доступного уровня.
Уровень повышается только после commit необходимых построек и выполнения
условий населения/снабжения. Название или платёж сами по себе не повышают его.

Критерии переходов задаются таблицами плагина: фактическое жильё, занятые
работы, запас пищи на несколько хозяйственных циклов, склад материалов,
работающий рынок/управление, а для военной ветки — казарма и гарнизон.
Конкретные цены/длительности — данные баланса, не enum в Rust.

### 3.3 Хозяйство

Поселением управляет владелец с ролями steward и captain. Steward управляет
строительством/складами/работами; captain — наймом и приказами. Проверка прав
выполняется на сервере при каждой операции, включая повтор после открытия UI.

Население состоит из постоянных жителей, сгруппированных плагином в семьи.
Один житель занимает одно рабочее назначение либо военную службу.
Дом хранит вместимость и фактически назначенных жильцов, а не создаёт людей
по таймеру. Переселение/новое население требует реальной spawn/adopt операции.

Пища, дерево, камень, металл, оружие и деньги не появляются от числа зданий.
Производство подтверждается совершённой работой, рецептами и переносом
предметов. Налоги перераспределяют имеющуюся казну/доход, не печатают валюту.
Первое снабжение вносит игрок; торговые сделки требуют реальных контрагентов
или явно ограниченного игрового источника, определённого правилами плагина.

Хозяйственные циклы используют simulation ticks. Выгруженное поселение не
получает бесплатно рассчитанный по wall clock урожай, опыт или завершённые
стройки. Видимая причина паузы: unloaded / no_workers / missing_input /
blocked_route / interrupted. За уже подтверждённую работу ресурсы не списываются
повторно после рестарта.

### 3.4 Рекруты и гарнизон

- Найм из конкретного живого взрослого жителя, а не из ближайшего нового NPC
  при каждом восстановлении связи. Нельзя нанять одного жителя двум владельцам.
- Ополчение забирает работника из хозяйства; увольнение возвращает **того же**
  жителя к гражданской жизни, сохраняя предметы по правилам демобилизации.
- Начальные роли: militia, infantry, spearman, archer. Название роли не
  заменяет реальное оружие, дальность атаки, боеприпасы и доступные действия.
- Обучение требует времени, казармы и снабжения; боевой опыт начисляется только
  по подтверждённым событиям боя. Уровни не делают пустую руку луком.
- Отряды и гарнизон являются группами одних и тех же сущностей, не отдельными
  копиями. Содержание расходует пищу/казну, нехватка снижает готовность и
  блокирует новые услуги; не убивать NPC скрытым таймером долга.
- Приказы: follow, move, hold, patrol, garrison, attack, retreat, dismiss.
  Строи: line, column, wedge, square. Hold сохраняет пост и локальную защиту;
  retreat отменяет преследование; patrol реально обходит заданные точки.
- Режимы целей: defensive и explicit_attack. Дружественные жители, союзники и
  свои отряды не становятся врагами из-за близости; PvP подчинён настройкам
  сервера и правам, а не клиентскому флагу.
- Гарнизон реагирует на угрозу, защищает жителей, возвращается на пост;
  дальние бойцы используют линию видимости и настоящие снаряды/боеприпасы.

Полная политическая симуляция королевств, осадные машины и тактическая камера
со свободным полётом не вытекают автоматически из слова «вайб». Этот контракт
требует развиваемых поселений и управляемых отрядов, не клона трёх игр целиком.

## 4. Общие правила новых core API

В этом документе все сигнатуры ниже **предлагаемые**, сейчас недоступны.
Имена аргументов — контракт DTO, не доступ к Rust-объектам.

- `request_id`: существующая script-id грамматика, максимум 64 байта.
- `operation_id`: долговечная owner-scoped identity по той же грамматике;
  `request_id` коррелирует доставку, `operation_id` — одну игровую операцию.
- Owner берётся только из admitted plugin, actor — из аутентифицированной
  сессии. UUID/координаты из UI сами по себе не дают полномочий.
- Версии/счётчики в Luau — целые в диапазоне `0..2^53-1`; UUID, hashes и
  opaque handles — строки. Нельзя передавать u64 через округлённый double.
- Результат: `request_id`, `operation_id` для мутаций, `state`, `revision`,
  `failure`, typed payload. Схема каждого результата закрытая.
- `state`: rejected / accepted / running / paused / committed / cancelled.
  `accepted` означает постановку, **не** готовое здание или выполненный удар.
- Повтор одного operation_id с тем же canonical fingerprint возвращает
  сохранённый результат; с другим — `operation_conflict`, без эффекта.
- До commit ошибки явные: invalid_request, forbidden, stale_revision,
  not_found, unloaded, blocked, insufficient_items, capacity, busy,
  runtime_unavailable. Области уточняют код, но не возвращают false/nil без причины.
- Неопределённый исход после durable append не объявляется отказом. Операция
  восстанавливается из журнала и сверяется запросом состояния.
- `solaris.operation_status(request_id, operation_id)` возвращает owner-scoped
  сохранённый outcome любой мутации; missing result не требует повторного
  эффекта. При неизвестном id возвращается not_found, не guessed rejected.
- Targeted результаты и изменения состояния доставляются владельцу; повторная
  доставка допустима, повторный игровой эффект нет. Рестарт требует snapshot
  и восстановления курсора, не доверия исключительно памяти обработчика.
- Все операции bounded; нет all-world сканов, неограниченных списков,
  генерации в Lua на worldgen worker, блокирования регионов на клиентском UI.
- Capabilities/events/commands и требуемые расширения проверяются strict при
  старте. `api = "0.6.0"` само по себе не подтверждает наличие новых операций.
  Предлагается закрытый `required_features` в manifest; неизвестный или
  отсутствующий feature блокирует загрузку, а не включает декоративный fallback.

Предлагаемые feature/capability группы (не новые cross-plugin services):
`world_sites`, `structure_operations`, `persistent_residents`, `resident_work`,
`resident_orders`, `inventory_transfers`, `storage_batches`, `client_views`,
`client_world_input`. Каждая группа разрешает только соответствующие вызовы
ниже. Results targeted и не требуют broadcast-подписки; notifications
`settlement.site_changed`, `resident.changed`, `resident.work_changed`,
`resident.order_changed`, `resident.combat_committed`, `structure.changed`
проверяются как owner-scoped subscriptions. Каждое notification содержит
стабильный event id и revision, пригодные для dedup и snapshot reconciliation.

## 5. Core: генерация, чертежи и runtime-стройка

### 5.1 Startup-каталог и выбор поселений

Предлагаемый selector: `settlement_profile = "feudal_settlements"`.
Он заменяет prototype в новом deployed set, не сосуществует с ним.
Сам движок не содержит правил «когда стать лордом»: при старте плагин
предоставляет проверенные определения вариантов и чертежей.

Новый manifest-контракт должен содержать ссылки на авторский каталог:
шаблоны, варианты размеров, biome/resource predicates, веса, ограничения
уклона, footprint, дороги и размещение inhabitants. Core вычисляет кандидатов
детерминированно по seed/profile revision/координатам и materializes план
независимо от порядка генерации чанков.

`Blueprint` schema 1:

- owned id, revision/content hash, bounding box, anchor;
- palette: namespaced block ids + закрытые registry-validated properties;
- bounded blocks с локальными координатами; quarter-turn rotation 0/90/180/270;
- входы, POI home/work/meeting/guard, точки соединения улиц;
- construction stages с наборами блоков; точные volume/cell лимиты;
- отдельные руинированные варианты и restoration stages;
- whitelist начального состояния block entities: пустые контейнеры и
  необходимые данные кровати/знака; никакого произвольного NBT/loot/команд.

Для stairs/doors/beds/fences и других зависимых состояний поворот должен
использовать свойства registry и корректно переносить связные группы.
Не «все facing заменить на north»; многочастный объект нельзя commit наполовину.

Предлагаемые стартовые лимиты: 128 blueprints, 64 варианта поселения,
128 размещений зданий на поселение; один blueprint до 65,536 блоков и
bounding box до 64×64×64. Каталог до 16 MiB decoded, проверяется вне Luau VM.
Это отдельные квоты от 16 MiB памяти VM. Полный город не упаковывается
в один мегашаблон или один Lua callback.

Нужно расширить package discovery для `structures/*.toml` (авторские данные)
и согласовать whitelist авторских UI/asset sources. Проверенные client ZIP
уже поддерживаются текущим core; добавлять второй механизм загрузки не нужно.
Исходный пакет хранит только авторские данные, а deployment-сборка создаёт
bundle в отдельном выходном каталоге и записывает его hash/size в deployed
manifest. Generated ZIP, Mojang bytes и runtime state не коммитятся сюда.
Уточнение package rules для новых авторских файлов — часть cutover, не обход.
Серверные blueprints не передаются как исполняемый клиентский код.

Bootstrap генерации должен иметь явный обратный путь в плагин:

```text
solaris.list_settlement_sites(request_id, cursor, limit)
solaris.query_settlement_site(request_id, site_id, cursor, limit)
solaris.reserve_resident_site(request_id, operation_id, site_id, poi_id,
    expected_site_revision)
solaris.release_resident_site(request_id, operation_id, spawn_site_token)
```

Страницы до 64 записей, owner-scoped snapshot/cursor. В persistent site snapshot
входят deterministic site id, variant/revision, footprint, здания/POI и
inhabitant generation ids. Идентификатор жителя образуется из world identity,
site id и inhabitant slot; materialization регистрирует постоянный resident
handle и связь generation id → UUID до публикации notification. Повторная
генерация/установка чанка не создаёт второго жителя. Plugin CAS по generation id
восстанавливает свои записи; потерянное notification восполняется scan.
Жители созданного профильным генератором поселения сразу принадлежат plugin
namespace, но это не означает назначенного игрока-владельца или военной службы.

`reserve_resident_site` выдаёт durable spawn_site_token для свободного
завершённого home POI. Token резервирует одно место, потребляется одним
spawn operation и освобождается подтверждённой отменой либо заселением.
Snapshot показывает свободные, зарезервированные и занятые места; один POI
не может заселить двух жителей по одному token.

### 5.2 Доступность участка

Предлагаемые вызовы:

```text
solaris.survey_site(request_id, dimension, bounds, purpose)
solaris.prepare_structure(request_id, operation_id, blueprint_id,
    anchor, rotation, survey_token, expected_site_revision)
solaris.advance_structure(request_id, operation_id, structure_id, stage,
    reservation_ref, expected_revision)
solaris.pause_structure(request_id, operation_id, structure_id, expected_revision)
solaris.cancel_structure(request_id, operation_id, structure_id, expected_revision)
solaris.structure_status(request_id, structure_id)
```

`survey_site` возвращает bounded snapshot: высоты/уклоны, воду, пригодные
участки, biome/resource tags, существующие защищённые зоны/постройки,
доступность чанков, revision и owner-scoped короткоживущий survey_token.
Максимум один участок 128×128 колонок на запрос; большие поселения обследуются
по связанным участкам. Истечение токена — перепроверка, не разрушение проекта.
Snapshot не загружает бесконечную область; `unloaded` сообщает, что не обследовано.

`prepare_structure` проверяет blueprint, геометрию, пересечения, права,
свободный путь к входу и связь с дорогой. Возвращает постоянный structure_id,
зарезервированный footprint, план этапов, объём работ и revisions.
Ничего не строит и не списывает деньги.
Права проверяются и при исполнении: чужой claim не обходится правами плагина.

Планировка не допускает дверей в скале, дорог через непреодолимый разрыв,
висящих полов и бесконечных cobblestone-столбов. Допустимая терраса/фундамент
ограничены сметой. Мосты имеют опоры/береговые соединения; если маршрут нельзя
построить в бюджете, он отклоняется, а не проходит по воздуху.

Межпоселенческая дорога строится между детерминированными соседними кандидатами
в ограниченном радиусе; пара имеет один canonical edge id, не две дороги.
Чанки размещают только собственный участок общего плана. Нет рекурсивной
загрузки соседей ради поиска следующего города. Исправлять природный мир
бесконечными тоннелями не требуется: непроходимое ребро отклоняется явно.

### 5.3 Commit, затраты и восстановление

`structure_id` адресует проект, `operation_id` — отдельную неизменяемую
мутацию prepare/advance/pause/cancel. Каждая следующая мутация получает новый
operation_id; повтор прежней использует прежний fingerprint.

Плагин хранит смету, финансовый резерв и intent в storage. Физические
материалы остаются в canonical inventories core: plugin ledger содержит
ссылки/проекции, не вторую авторитетную копию содержимого склада.
Core предоставляет durable inventory reservation из раздела 6.2.

Протокол:

1. Подготовить участок; при отказе ничего не списывать.
2. CAS plugin ledger и construction intent; затем зарезервировать реальные
   материалы через `reserve_inventory_items`. Intent хранит id обеих операций,
   поэтому crash между ними восстанавливается, а не объявляется атомарным.
3. `advance_structure` связывает structure/stage с reservation_ref и immutable
   resource_plan_hash. План задаёт затраты на каждую work portion, максимум
   16 видов ресурсов; plugin определяет смету, core проверяет наличие резерва
   и запрещает использовать одну его долю в двух проектах.
4. Core исполняет этап через назначенных строителей. Каждая порция получает
   монотонный receipt sequence, точный вектор consumed materials и work units.
   World edits, потребление этой доли inventory reservation и receipt должны
   иметь **единый восстанавливаемый commit-протокол** C1/C2/C4: после crash
   нельзя оставить блоки без расхода либо расход без выполненной порции.
   Отдельные несогласованные журналы эту гарантию не выполняют.
5. Plugin применяет receipts последовательно/idempotently; завершённый stage
   открывает POI/вместимость. Crash после физического commit до plugin CAS
   повторяет проекцию, не работу. Полностью завершённый stage не является
   минимальной единицей расчёта расхода.
6. Pause/cancel сериализуются с активной порцией. Ответ содержит окончательный
   receipt watermark и consumed/remaining; только после сверки этого watermark
   разрешён возврат оставшегося резерва. Для каждого ресурса:
   reserved = consumed + returned + remaining.
7. Уже построенное остаётся в мире; снос — отдельная смета и проверяемые блоки,
   не rollback поверх работы игрока. Pause сохраняет проект/резерв; новый
   advance продолжает с последнего receipt. Cancel терминален для structure_id.

Предлагаемый предел одной world commit-порции: 512 блоков, с целостностью
зависимых групп; bounded очередь 64 активных строительных операций на plugin.
Проверки preconditions и journaling должны учитывать cross-chunk/region этапы.
При изменении игроком блока в footprint операция останавливается как
`paused/site_changed`, сохраняет расход и не затирает постройку игрока.

Резервы после рестарта сверяются по structure_id, operation_id и reservation_ref.
Неизвестная core операция может быть повторно отправлена из durable intent;
возврат денег/материалов разрешён лишь после подтверждённого terminal
cancel/reject и окончательного receipt watermark.

## 6. Core: постоянные жители, работы и войска

### 6.1 Идентичность и жизненный цикл

```text
solaris.claim_resident(request_id, operation_id, actor_id, entity_uuid,
    expected_entity_revision)
solaris.spawn_resident(request_id, operation_id, spawn_site_token, profile)
solaris.query_residents(request_id, handles, cursor)
solaris.release_resident(request_id, operation_id, handle, expected_revision)
solaris.set_resident_pois(request_id, operation_id, handle,
    home_poi, work_poi, meeting_poi, expected_revision)
```

`claim_resident` проверяет живого допустимого NPC, дистанцию/измерение actor,
отсутствие другого owner и revision. Результат содержит постоянный opaque
handle, entity UUID, revision и snapshot. Handle адресует того же NPC после
перезапуска/перемещения между регионами, но не является обходом owner ACL.
Чужой plugin с украденной строкой получает forbidden.

Три разных понятия не смешиваются: core plugin owner; plugin settlement/player
authorization; exclusive civilian/military assignment. Для найма уже
принадлежащего плагину NPC повторный claim не нужен. Плагин сначала CAS
resident assignment `civilian → recruiting(operation_id, actor, squad)` вместе
с платёжным intent; конкурирующий игрок в **том же** namespace получает conflict.
После core admission intent финализируется в military; до этого другое
назначение запрещено. Crash после admission восстанавливается по operation
receipt, не повторным списанием/наймом. Отказ возвращает assignment только
после подтверждённого terminal результата и возврата допустимого резерва.

`spawn_resident` — отдельная операция заселения, привязанная к зарезервированному
spawn site/POI; повтор не создаёт дубликата. `profile` — bounded данные
разрешённого типа жителя, не произвольная таблица характеристик босса.
Сам факт наличия дома не даёт клиенту права вызвать spawn.

`set_resident_pois` связывает того же жителя с существующими завершёнными
owner-scoped POI и проверяет их вместимость/назначение. Nil снимает конкретную
связь. Разрушение дома/рабочего места инвалидирует POI и публикует изменение;
brain не продолжает считать несуществующую кровать доступной. Выбор семьи/
работы остаётся политикой плагина, а physical POI validity — authority core.

Состояния: alive_loaded, alive_unloaded, dead, released. Умерший остаётся
тяжёлым tombstone для dedup/учёта, но не занимает место живого населения.
Unloaded не означает dead и не разрешает заменить бойца ближайшим жителем.
Конверсия/удаление сущности публикуют отдельный lifecycle result; плагин
останавливает назначения, не привязывает запись молча к другой сущности.

`query_residents` принимает до 64 handles, возвращает по handle health,
position при loaded, equipment summary, work/order state, lifecycle и revision.
Подписка на owner-scoped изменения передаёт только изменившиеся записи;
пропуск/рестарт восстанавливается snapshot. Session disconnect не увольняет
гарнизон и не снимает владение. Удаление plugin не передаёт NPC другому owner;
оператор должен выполнить явный release/migration.

### 6.2 Предметы и физические работы

```text
solaris.query_owned_inventory(request_id, endpoint, expected_revision)
solaris.transfer_owned_items(request_id, operation_id, actor_id,
    transfers, expected_revisions)
solaris.reserve_inventory_items(request_id, operation_id, endpoint,
    resource_plan, expected_revision)
solaris.inventory_reservation_status(request_id, reservation_ref)
solaris.release_inventory_reservation(request_id, operation_id,
    reservation_ref, expected_revision)
solaris.assign_resident_work(request_id, operation_id, handle,
    work_order, expected_revision)
solaris.cancel_resident_work(request_id, operation_id, handle,
    expected_revision)
```

Inventory endpoint — tagged union player_inventory / resident_equipment /
resident_carry / warehouse. Resident endpoint содержит owned resident handle;
warehouse endpoint — завершённый container POI handle из site snapshot.
Snapshot даёт bounded canonical slots, item summaries и revision; никакого
произвольного NBT или доступа к сундуку только по присланным координатам.

Каждый transfer содержит source endpoint/slot, destination endpoint/slot и
count; максимум 16 переносов. Поддерживаются оба направления player ↔ NPC,
warehouse ↔ NPC, player ↔ warehouse. Core проверяет права, доступность,
дистанцию соответствующего actor/worker, слоты, количества и revisions
**всех** сторон до одного recoverable transfer commit. Компоненты и прочность
сохраняются. Предмет после crash не может одновременно лежать у игрока и NPC.

Reservation блокирует bounded реальные стеки от перемещения/расхода другой
операцией и возвращает reservation_ref, resource_plan_hash, quantities/revision.
Расходует его только привязанная world/work operation по committed receipts.
Release сериализован с потреблением и возвращает только remaining; занятый
или недоступный destination даёт паузу/отказ, не уничтожение предметов.
Связь reservation с construction receipts задана в разделе 5.3.

`work_order` — закрытый tagged union физических действий: harvest/replant,
cut_tree, mine, haul, craft, fish, tend_livestock, construct. Каждое имеет
конкретную цель/ограниченный участок, источники/приёмники, инструмент и
допустимый рецепт. Construct ссылается на подготовленную operation/stage.
Плагин выбирает работу; core исполняет через действующие механики добычи,
рецептов, инвентарей, перемещения и региональной авторитетности.

Не превращать job title в пассивный генератор ресурсов. Fish использует
доступную воду и каноническую механику добычи; mining не ищет руду по всему
миру; cut_tree не разрушает чужие деревянные дома; ranching расходует корм
и работает с реальными животными. Protection и отсутствие tool/input/route
останавливают задание с typed причиной. Результат содержит только реально
совершённые work units и inventory changes, не обещанный выпуск.

### 6.3 Приказы отрядам

```text
solaris.issue_resident_order(request_id, operation_id, handles,
    expected_order_revisions, order)
solaris.cancel_resident_order(request_id, operation_id, handles,
    expected_order_revisions)
```

До 64 handles за один приказ; gameplay squad до 32 бойцов. Приём проверяет
всех членов, права и revisions перед заменой orders; частичная недоступность
отклоняет batch с причинами по членам. После приёма pathing/fighting каждого
бойца могут дать разные runtime-результаты, явно видимые в UI.

Линеаризация batch — durable group-admission record. Региональные owners
подготавливают ownership/order-revision fences без частичного исполнения.
До записи commit остаются старые orders; после записи логически приняты все
новые, а каждый owner применяет их один раз при replay. Миграция/unload/death
между prepare и commit либо инвалидируют prepare целиком, либо происходят
после общего commit и дают обычный per-member execution outcome.
Те же правила действуют для cancel. Нельзя реализовать all-or-nothing простым
циклом независимых `move_villager_to` и последующей компенсацией.

`order` — закрытый union:

| Kind | Поля | Наблюдаемое поведение |
| --- | --- | --- |
| follow | authenticated player target, formation, spacing | Отряд сопровождает движущегося игрока без необходимости ломать блоки для refresh |
| move | dimension, anchor, heading, formation, spacing | Доступные индивидуальные позиции строя, без телепортации |
| hold | anchor, heading, formation, engagement radius | Держит пост, защищается в радиусе, не преследует бесконечно |
| patrol | 2–16 waypoints, engagement radius | Обходит маршрут, возвращается к нему после угрозы |
| garrison | approved post/POI handles, engagement radius | Занимает свободные посты и защищает поселение |
| attack | server-resolved target refs, engagement policy | Настоящая атака, дальность/LOS/cooldown/боеприпасы/смерть |
| retreat | safe anchor, formation | Прерывает attack/chase, движется к точке отхода |

Демобилизация — отмена военного order, возврат экипировки через
`transfer_owned_items` и CAS assignment `military → demobilizing → civilian`.
При недоступном складе состояние остаётся demobilizing без потери вещей.
Она сохраняет resident handle, жильё и plugin ownership; `release_resident`
используется только для полного отказа от владения, не обычного увольнения.
При logout follow-target order переходит к назначенному garrison, а без него
к hold на последней допустимой позиции; logout не оставляет вечное chase.

Formation slot assignment стабилен между обновлениями, учитывает footprint
сущностей, препятствия и уступание пути; невозможный строй сообщает
`blocked_route`, а не копит бойцов в одной координате. Позиции рассчитываются
движком, не Lua-циклом команд на каждую сущность каждый tick.

Плагин передаёт bounded policy союзников/врагов с revision; core фильтрует
цели в уже доступном локальном perception. No all-world enemy scans.
Боевое событие содержит attacker/victim handles, committed damage/kill и
корреляцию активного order, чтобы плагин мог начислить опыт один раз.
Не имитировать оружие частыми `damage_entity` из Luau в обход боевой механики.

Target ref не равен управляемому resident handle. Он выдаётся core из
проверенного world-selection context либо локального perception и может
адресовать hostile/игрока без права управления им. Он содержит bound world/
entity identity, допустимую policy и срок; поддельный/устаревший ref отклоняется.
Engagement policy задаётся внутри order: stance, ally player UUIDs / owned
resident affiliation ids (суммарно до 64), permitted hostile categories и
policy_revision. Изменение policy — новый приказ с revision fence; игроки
по умолчанию не цель. Core всегда дополнительно применяет server PvP rules.

## 7. Core: storage и crash-безопасность экономики

Нужен bounded `storage_batch_cas(request_id, operation_id, mutations)`:
до 16 ключей с existing CAS semantics, одна ревизия/один durable результат,
all-or-nothing для plugin ledger + operation intent + назначения жителей.
4096 bytes на значение не увеличивать ради монолитного state всего мира.

Нужен `storage_scan(request_id, prefix, cursor, limit)` для восстановления
индексов/операций после рестарта: максимум 64 ключа за страницу, cursor
owner-scoped с фиксированной snapshot revision и сроком жизни. Повтор/сбой
страницы не теряет записи; истечение требует нового scan. Не вводить чтение
чужого plugin namespace.

До платного найма/внесения материалов обязательно закрыть документированный
разрыв `inventory_storage_transaction`: durable recovery intent для playerdata
и storage, повторный replay не дублирует предметы/оплату. Тот же критерий
относится к transfer предметов между player, NPC и физическим warehouse.
Проверять процессный crash, а не только обычный отказ API.

Предлагаемые plugin records, все с schema/revision:

- `settlement:<id>`: owner/permissions reference, site, size/stage/branch,
  specialization, compact ledger и population/building counts;
- `building:<id>`: settlement, blueprint revision, transform, stage, POI,
  core structure_id, condition;
- `resident:<id>`: settlement/family, постоянный core handle, job либо squad,
  recruitment/training intent; health и equipment не имеют второго authority;
- `squad:<id>`: settlement, commander, handles, policy/order reference;
- `operation:<id>`: kind, reserved/consumed amounts, stage receipts,
  committed/paused/cancelled state;
- bounded shard indexes, не одна бесконечно растущая JSON/pipe-строка.

Ограничение сейчас — 4,096 live records на plugin. Admission вычисляет
вместимость до заселения/стройки и резервирует место для operation records.
Завершённые receipts могут компактизироваться только с сохранением dedup
семантики; удалённая запись не должна снова сделать старую оплату допустимой.

## 8. Loader: интерфейсы, ввод и доверие

### 8.1 Общий протокол

Текущие schema 1 / wire 2 не изображают произвольный RTS UI. Предлагается
явный cutover на **bundle schema 2 / wire 3** в core и всех трёх adapters,
с миграцией существующих bundle fixtures, без параллельного старого decoder.
Plugin API остаётся 0.6.0; номера относятся к другим контрактам.

Нужны generic закрытые виджеты: paged table/list, tabs, bounded number/text
input, enum selection, resource/cost panel, action button, world marker.
Это не HTML/JS, arbitrary Java, filesystem access или исполнение Lua клиентом.
Screens декларативные, экономическая/военная политика остаётся на сервере.

Предлагаемые API жизненного цикла представления:

```text
solaris.open_client_view(request_id, player_id, owned_view_id, model)
solaris.present_client_view(player_id, view_instance_id, expected_revision, model)
solaris.close_client_view(player_id, view_instance_id)
solaris.begin_client_selection(request_id, player_id, view_instance_id,
    view_revision, action_id, constraints)
solaris.cancel_client_selection(player_id, selection_context_id)
on_loader_view_action(event)
```

`model` содержит только объявленные в bundle поля/виджеты, данные текущей
страницы и разрешённые actions. Пакет ограничен 64 KiB, до 64 строк/страницу,
16 полей формы и 16 actions. Каждое число конечное; строки имеют byte bounds.
Полный roster города не передаётся каждый tick.

`open_client_view` регистрирует на сервере новую instance и возвращает её
opaque id/revision; instance связана с точной live session, plugin owner,
verified view definition, action whitelist и typed field schema текущей модели.
Present CAS-заменяет модель/revision, инвалидируя прежние action/context.
Close от сервера или подтверждённый close от клиента удаляет instance и все
selection contexts; delivery в уже закрытую instance запрещена.

Schema 2 сохраняет явные content/permission пары; новые пары:
views/present_views, view_actions/send_view_actions,
world_previews/present_world_previews, world_selection/send_world_selection.
Последняя требует разрешённого view action; permission проверяется и при
handshake, и на ingress конкретного action. Verified assets требуют прежнего
load_assets. Для entity presentation при необходимости отдельная пара
entity_presentations/present_entities. Нет неявного разрешения «всё через UI».

Action DTO: view_instance_id, view_revision, action_id, action_sequence,
bounded typed fields, selection_token при world selection. Actor/connection
подставляет сервер. Sequence dedup связан с конкретной session/view, а не
является постоянным operation id. Плагин создаёт durable operation id сам.

Устаревшие revision, закрытый экран, чужой owner, отключённая кнопка,
подставленные id/цены/количества не разрешают действие. Сервер заново читает
права, цену, ресурсы и объект. Ошибка показывает причину и обновляет модель.
Повтор клика при неопределённом исходе запрашивает ту же operation, не покупает
второй отряд. Logout очищает только UI/selection state, не гарнизон.

### 8.2 Обязательные экраны

| Экран | Данные | Действия |
| --- | --- | --- |
| Поселение | Размер, стадия/ветка, люди, жильё, еда, казна, причины остановки | Выбор поселения, управление правами, переход в разделы |
| Застройка | Каталог, footprint preview, смета, требования перехода | Survey, rotation, разместить проект, очередь, pause/cancel/resume, восстановление руин |
| Хозяйство | Семьи/работы, склады и фактические входы/выходы | Назначение работ, внесение/изъятие предметов, снабжение, ограниченные налоги |
| Гарнизон | Посты, бойцы, подготовка, потребление | Найм выбранного NPC, экипировка, обучение, назначение/снятие с поста, увольнение |
| Армия | Отряды, состав, здоровье/готовность, текущий order | Выбор группы, строй, stance, move/follow/hold/patrol/attack/retreat |
| HUD | Выбранный отряд, order, подтверждение/ошибка, доступные клавиши | Не перехватывает фокус; подробности через экран |

Loader оснащение должно отображаться на сущности, а не только в таблице.
Core передаёт canonical equipment metadata; если визуальный образ/анимации
вооружённого villager требуют клиентского renderer, Loader получает отдельную
закрытую entity-presentation схему, verified assets и разрешение владельца.
Никаких динамических entity registry ids от клиента в world storage.
Это обязательный upstream результат, если vanilla renderer не показывает
проверенные типы оружия/брони и действия корректно.

### 8.3 Выбор на местности

Предлагаемые действия: выбрать сущность/отряд, выбрать точку приказа,
добавить waypoint, повернуть и подтвердить preview. Клиент рисует ghost
blueprint, радиусы и formation markers, но не размещает блоки самостоятельно.

World selection проходит через server-issued context: owner, action kind,
срок, допустимая dimension/range, view revision. Клиент передаёт намерение и
ray/target; сервер проверяет текущую authoritative pose, LOS, дальность,
claim, видимость/принадлежность цели и повтор контекста. Произвольная координата
не разрешает строить или атаковать через полмира.

`begin_client_selection` выдаёт opaque context id, привязанный к точной
session/owner/view/action/revision, ограничениям и expiry simulation tick.
Одна принятая точка потребляет context; следующий waypoint получает новый.
Производное target_ref либо position proof создаёт сервер после проверки,
не сам клиент. Повтор уже потреблённого context возвращает тот же admission
result без нового события/эффекта. View replacement/close, disconnect и
отзыв plugin permissions инвалидируют contexts. Отзыв доменной роли обязан
закрыть/обновить views плагином; даже до доставки закрытия проверка прав
на сервере не позволяет выполнить старое действие.

Preview использует owner/hash-verified визуальную проекцию server blueprint
из bundle (блоки/transform, без backend функций). C2 и L1 проверяют одинаковый
content hash/геометрию; отсутствие проекции блокирует preview/подтверждение,
а не показывает произвольный «похожий» дом.

Клавиши по умолчанию: V — поселение, R — отряды; игровые действия
переназначаемы в Controls. Выбор цели — только в явно включённом command mode;
Escape отменяет режим. Смена окна, чат, инвентарь, disconnect и отзыв прав
сбрасывают held/selection state. Нельзя одновременно провести обычную атаку
и команду отряду одним перехваченным кликом.

Fabric / NeoForge / Forge используют одну модель/validator/presenter;
платформенные adapters отвечают за native registration, input и lifecycle.
Несовместимый/отсутствующий Loader отклоняется в Configuration с указанием
нужной версии/permissions, до допуска в Play. Никакого server-only fallback
для этого пакета.

## 9. Пакет и переход со старых плагинов

Чистый cutover: `colony-villager-scaffold` + `settlement-prototype` →
`solaris-settlements`; один manifest, storage owner, набор commands и bundle.
Старые пакеты удаляются из deployed set только в финальной миграции.
Не оставлять aliases, вторую authority или две копии persistent roster.
`solaris-towns`, permissions/economy и пять членов standard-pack не меняются
без отдельной необходимости; новый пакет не вызывает их сервисы.

По умолчанию — **новый world directory**: текущий core закрепляет owner и
ordered settlement plan в `solaris/world.json`; смена плана в старом мире
небезопасна. Никогда не стирать мир, не переписывать owner вручную и не
генерировать новый профиль поверх старых чанков автоматически.

Если требуется перенос данных, нужен отдельный явный offline migration
инструмент core с dry-run, backup и отчётом. Он переносит plugin-owned intent
в новый namespace, но не выдумывает entity UUID: старый scaffold сохраняет
status/role/order/generation, а не постоянную идентичность бойца. Такие записи
помечаются как требующие ручного сопоставления, не заменяются ближайшим NPC.
Этот дополнительный инструмент не является обязательным условием новой игры.

Core-owned ссылки исправляются **в core**: `docs/PLUGINS.md`,
`docs/COLONY_SPEC.md`, deployment examples, harness `plugin_examples` и другие
найденные тесты/fixtures старых id. Loader-owned examples/fixtures — в Loader.
Здесь меняются package directories, root README и собственные инструкции
разрешённых артефактов после согласованного upstream schema cutover.

## 10. Очередь upstream-задач

Это зависимости поставки, не разрешение объявить отдельный этап готовым overhaul.
Каждая задача принимает свои API/поведение и мигрирует все затронутые callers.

| ID | Владелец и целевая область | Результат | Зависимости |
| --- | --- | --- | --- |
| C1 | Core: `mc-script`, storage/player inventory adapters | Batch CAS, scan, receipts, bidirectional transfers/reservations, crash recovery предметов/денег | Нет |
| C2 | Core: `mc-script/src/lua.rs`, `mc-worldgen/src/structures.rs`, world commits | Catalog/schema, размеры/дороги, site discovery/POI, survey/prepare/staged construction с per-portion receipts | C1 и C3; construct worker integration закрывается C4 |
| C3 | Core: `mc-script`, `mc-net/src/script/villager.rs`, regional entity owner | Постоянные handles, lifecycle snapshots/events, generic adopt/spawn/release identity; interface для worldgen bootstrap | C1 для durable paid-operation intent; не зависит от site catalog C2 |
| C4 | Core: canonical inventories, entity work/combat/AI | Реальные работы, equipment, cross-region group admission/orders и formations | C3; C1 для transfers; C2 для construct; общий consumption receipt contract с C1/C2 |
| L1 | Loader: `loader-core`, `loader-platform-common`, три adapters; core script UI endpoint | Schema 2 / wire 3, instance/context lifecycle, формы/input/preview, equipment presentation | Wire/manifest контракт до edits; разработка не зависит от готовых C2/C4, совпадающий blueprint projection hash обязателен при интеграции |
| P1 | Plugins: новый объединённый пакет и собственные assets | Доменные данные, рост/экономика/найм/отряды, Loader views, blueprints | C1–C4, L1 |
| I1 | Core harness + Loader scenarios + deployed package | Strict deployment, миграция ссылок, реальный клиент, restart/adversarial/scale gates | Все предыдущие |

Чтобы не создать цикл: C1 реализует общий inventory transfer/reservation
commit и player/warehouse endpoints; C4 подключает resident endpoints на
основе handles C3. C3 реализует идентичность и idempotent entity materialization;
C2 связывает их с site catalog, выдачей spawn token и site-проверкой публичного
`spawn_resident`. C2 принимает stage reservations/receipts, C4 добавляет
исполнение work units строителями. Ни C2 без C4, ни C1 без resident integration
не считаются полной игровой стройкой или экипировкой.

Rust root orchestration files только маршрутизируют. Новые state machines
живут в профильных модулях; не добавлять «FeudalGameManager» в server.rs.
Совместные DTO/границы должны быть закреплены до параллельных изменений,
иначе авторы core, Loader и plugin реализуют разные протоколы.

## 11. Проверяемая приёмка

Ни один пункт ниже сейчас не выполнен. Это задания для будущих исполняемых
сценариев. Проверки структуры manifest/DTO не заменяют игру.

### 11.1 Core, восстановление и безопасность

| ID | Сценарий | Обязательный результат |
| --- | --- | --- |
| A01 | Strict load нового пакета; затем missing feature, второй selector, чужой blueprint, stray file, hash mismatch | Валидный пакет загружается; каждый невалидный вариант fail-closed до игровых эффектов |
| A02 | Одни seed/profile, разный порядок генерации; повтор site discovery, crash adoption и restart | Одинаковые layout/inhabitant ids, блоки и дороги; bootstrap даёт те же resident handles без дубликатов/швов |
| A03 | Плоский участок, берег, склон, овраг, чужой claim, игрок строит после survey | Валидные участки принимаются; остальные отклоняются/перепроверяются без затирания |
| A04 | Crash до/после reservation, внутри work portion, посреди stage, после physical commit до plugin receipt | Те же блоки/ресурсы после replay; нет двойного расхода, бесплатного возврата или повторного здания |
| A05 | Отмена до первого блока, посреди portion/stage и после стадии; site_changed одновременно с cancel | Final watermark фиксирует consumed; возврат только remaining, сохранение построенного и баланса ресурсов |
| A06 | Два игрока одного plugin namespace нанимают одного жителя; crash после core admission; unload/reload/restart, смерть | Одно military assignment/оплата; тот же UUID возвращается; смерть не заменяет NPC и не повторяет списание |
| A07 | Player ↔ NPC ↔ warehouse ↔ player, inaccessible/full destination; kill process на durable границах | Каждый предмет ровно в одном месте; отказ ничего не теряет, компоненты/прочность сохранены |
| A08 | Harvest/plant/craft/haul без input/tool, затем со снабжением | Нет продукции без действий/входов; корректный расход и реальный предметный выход |
| A09 | Направить отряд через доступный проход и закрытый проход | Реальное движение/перестроение либо понятный blocked; без телепорта и перманентной общей точки |
| A10 | Стража против hostile, свой житель рядом, лучник без стрел/за стеной | Враг обрабатывается боевой системой; нет friendly fire из неправильного target selection, бесплатных стрел и ударов сквозь стену |
| A11 | Attack → retreat → patrol; logout; issue/cancel разных командиров на членах разных регионов, migration/death между prepare/commit | Одно общее решение admission; старое преследование прекращается; stale batch не меняет ни одного order |
| A12 | Crash между group prepare/commit/apply; full queues, новый payload старого operation id, поддельный handle, потерянный result | Replay применяет всё принятое один раз; непринятый batch сохраняет старые orders; outcome доступен через status |

### 11.2 Плагин: полный игровой цикл

| ID | Сценарий | Обязательный результат |
| --- | --- | --- |
| G01 | Посетить по одному естественному hamlet/village/town | Разные физические масштабы, заданный диапазон домов/жителей, пригодные входы и пути |
| G02 | Развить деревню по каждой из трёх веток на отдельных сохранениях | Настоящий манор/замок/город, сохранённое жилое окружение, подтверждённые затраты и рабочие функции |
| G03 | Попытка города в малом footprint, затем законное расширение | Нет повышения без земли; survey/строительство расширения открывают вместимость |
| G04 | Найти ruined fortification и восстановить | Не считается готовой до ремонта; расходуются материалы/работа, появившиеся посты доступны гарнизону |
| G05 | Farming, forestry, ranching, fishing, mining на подходящих и неподходящих участках | Разные рабочие постройки/действия; неподходящий участок не даёт фиктивное производство |
| G06 | Призвать, экипировать, построить отряд, демобилизовать; недоступный склад, рестарт, повторный найм | Та же личность и civilian ownership; вещи не теряются; возврат к работе/повторный найм не создают копию |
| G07 | Исчерпать пищу/материалы/деньги, восстановить снабжение, рестарт | Понятная остановка и возобновление; ни отрицательных запасов, ни offline gifts |
| G08 | Два steward/captain одновременно строят/нанимают; затем отозвать права | Нет двойного расхода/слота; старый открытый UI не сохраняет отозванные права |

### 11.3 Реальный Loader и визуальная проверка

| ID | Сценарий | Обязательный результат |
| --- | --- | --- |
| U01 | Первый вход, отказ permission, неверная версия, vanilla клиент | Корректное согласие либо явный Configuration disconnect, без частичной активации |
| U02 | Все шесть экранов; 80 жителей; несколько страниц; разные GUI scale/окна | Данные читаемы, списки не вылезают, selection стабилен, расходы соответствуют серверу |
| U03 | Preview повёрнутого дома/ворот/стены на границе чанков; подтвердить стройку | Preview совпадает с authoritative результатом; двери/лестницы ориентированы и проходимы |
| U04 | Выбор отряда/точки, waypoint, клавиши, чат/Escape/focus loss | Приказы доходят один раз, ordinary input не дублируется, нет застрявшего режима |
| U05 | Подмена fields/revision/цены/actor/target, чужой context, replaced/closed instance, повтор context, отсутствие permission, reconnect | Отказ или resync; session/view/permission fences сохраняются, клиент не получает authority |
| U06 | Все платформы Fabric/NeoForge/Forge, вооружённые NPC и реальные бои | Одинаковая семантика, видимое оружие/броня/действия, отсутствие protocol/render рассинхронизации |

Graphical gates — через canonical harness core и реальный Minecraft под Xvfb,
со screenshots/video и server receipts. Lua mock, browser-макет или TCP-only
тест не доказывают внешний вид замка, работу мирового preview или бой.

### 11.4 Размер и производительность

Приёмочная нагрузка: один город с 80 гражданскими и 64 бойцами в двух
отрядах, второе загруженное поселение, два игрока; одновременно бой,
перестроение и стройка. Отдельно — 256 загруженных жителей/бойцов суммарно.
Числа — целевой проверочный сценарий, не обещание Total War на тысячи NPC.

Сравнить debug-результат с тем же seed/сценой без активных работ; записать
simulation tick p50/p95/p99, queue saturation, CPU/memory, количество path
requests, world commit время, рост journal и сохранение после рестарта.
Для целевых 20 TPS steady-state p95 должен укладываться в 50 ms на
зафиксированном test host; p99/выбросы отдельно объясняются и не маскируются.
При непрохождении — реальное исправление/измерение, не уменьшение сценария
без согласия владельца. Проверить, что background/unloaded не исполняется
дорогим polling и что plugin handler остаётся в пределах 32 commands и
100,000 instructions.

## 12. Готовность к реализации и конечный критерий

Перед началом upstream edits закрепить DTO/schema из разделов 4–8 в задачах
C1–C4/L1 и назначить владельца каждой общей границы. Изменение proposed
лимитов допускается по измерению, но не скрытое исключение игровой ветки.

Контракт считается реализованным только после C1–C4, L1, P1, I1 и A/G/U
сценариев с приложенными receipts. Зелёный Cargo build, объединённый manifest,
красивый screenshot без хозяйства либо таблица `tier=castle` не являются
полноценным settlement overhaul.

Текущая поставка: техническое задание и карта зависимостей. Игровые пакеты,
ядро, Loader, существующие миры и deployment settings не изменены.
