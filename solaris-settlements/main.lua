--!strict

-- Solaris Settlements v1 — the single settlement/profile package (API 0.6.0).
--
-- Server data plus verified client content: the package also ships the Loader
-- bundle `client/settlements-ui.zip`, which declares one settlement screen, and
-- works with a vanilla client until that bundle is activated. Authored
-- blueprints live in `structures/*.toml` of this package (frozen schema 1).
--
-- Core surface consumed (docs/PLUGINS.md):
--   settlement sites + staged construction, persistent residents, resident work
--   and squad orders (C4), owned inventory transfers/reservations and the bound
--   warehouse container, durable batch storage.
--
-- Durable model: every record is a versioned string in plugin storage with a
-- schema tag and a plugin revision, sharded per settlement and bounded by
-- config plus a hard encoded-size check. Index records are derived projections
-- of the value records and are written in the same atomic storage batch, never
-- as a second authority.
--
-- Economy: nothing here mints resources. Construction consumes real items
-- through a C1 reservation bound to the core structure plan; verified supply is
-- a projection of a real `query_owned_inventory` read at a stated tick; the
-- treasury only moves through verified item transfers, and a tax only
-- redistributes an existing balance. A job title never produces items.
--
-- The client screen is a projection too: every displayed row and counter comes
-- from the durable settlement record, the derived resident index, or a live
-- `query_owned_inventory` read of the warehouse container core bound for this
-- package. Nothing is rendered from a fabricated row.
--
-- Core calls that do not exist yet are reported instead of faked
-- (see MISSING_CORE_CALLS near the end): package structure discovery plus the
-- `feudal_settlements` selector, and any money authority. Resident work, squad
-- orders and demobilisation are wired to the real C4 calls and report exactly
-- what core commits.

-- One table of module-level helpers: the main chunk must stay under the Luau
-- register limit, so the plugin's functions live in `S` instead of ~160
-- separate top-level locals.
local S: any = {}

local raw_config: any = solaris.config()

S.config_number = function(name: string, minimum: number, maximum: number): number
    local value: any = raw_config[name]
    assert(type(value) == "number" and value % 1 == 0, name .. " must be an integer")
    assert(value >= minimum and value <= maximum, name .. " is out of range")
    return value
end

S.config_items = function(name: string, maximum: number): { string }
    local value: any = raw_config[name]
    assert(type(value) == "table", name .. " must be an array of item ids")
    assert(#value >= 1 and #value <= maximum, name .. " must hold 1-" .. tostring(maximum) .. " item ids")
    local items: { string } = {}
    for index = 1, #value do
        local item: any = value[index]
        assert(
            type(item) == "string"
                and #item <= 64
                and string.match(item, "^[a-z0-9_.-]+:[a-z0-9_./-]+$") ~= nil,
            name .. " entries must be namespaced resource ids"
        )
        items[index] = item
    end
    return items
end

local MAX_SETTLEMENTS = S.config_number("maximum_settlements", 1, 64)
local MAX_BUILDINGS = S.config_number("maximum_buildings", 1, 64)
local MAX_RESIDENTS = S.config_number("maximum_residents", 1, 64)
local MAX_SQUADS = S.config_number("maximum_squads", 1, 16)
local MAX_PENDING_OPS = S.config_number("maximum_pending_operations", 4, 16)
local CYCLE_TICKS = S.config_number("cycle_ticks", 20, 630720000)
local SUPPLY_TICKS = S.config_number("supply_ticks", CYCLE_TICKS, 630720000)
local FOOD_ITEMS = S.config_items("food_items", 16)
local MONEY_ITEMS = S.config_items("money_items", 8)
assert(SUPPLY_TICKS >= CYCLE_TICKS)

-- ---------------------------------------------------------------------------
-- Bounded vocabulary
-- ---------------------------------------------------------------------------

local SIZE_CLASS: { [string]: boolean } = { small = true, medium = true, large = true }
local STAGE: { [string]: boolean } = { hamlet = true, village = true, developed = true }
local BRANCH: { [string]: boolean } = { none = true, estate = true, fortress = true, town = true }
local CONDITION: { [string]: boolean } = { inhabited = true, partially_ruined = true }
local POI_KIND: { [string]: boolean } = { home = true, work = true, meeting = true, guard = true }
local POI_STATE: { [string]: boolean } = { free = true, reserved = true, occupied = true }

local SPECS: { [string]: boolean } = {
    farming = true, forestry = true, ranching = true, fishing = true, mining = true,
}
local SPEC_WORKPLACE: { [string]: string } = {
    farming = "solaris:farm",
    forestry = "solaris:sawmill",
    ranching = "solaris:pen",
    fishing = "solaris:fishing_pier",
    mining = "solaris:mine_entrance",
}
local SPEC_TAGS: { [string]: { string } } = {
    farming = { "crops", "farmland", "grassland", "soil" },
    forestry = { "trees", "forest", "timber" },
    ranching = { "grassland", "pasture", "soil" },
    fishing = { "water", "ocean", "river", "shore" },
    mining = { "stone", "ore", "rock", "cave" },
}

local JOBS: { [string]: boolean } = {
    farming = true, forestry = true, ranching = true, fishing = true, mining = true,
    construction = true, hauling = true, crafting = true,
}
local JOB_WORKPLACE: { [string]: string } = {
    farming = "solaris:farm",
    forestry = "solaris:sawmill",
    ranching = "solaris:pen",
    fishing = "solaris:fishing_pier",
    mining = "solaris:mine_entrance",
    crafting = "solaris:smithy",
    construction = "solaris:warehouse",
    hauling = "solaris:warehouse",
}
local SERVICE: { [string]: boolean } = {
    civilian = true, recruiting = true, military = true, demobilizing = true,
}
local MILITARY_ROLES: { [string]: boolean } = {
    militia = true, infantry = true, spearman = true, archer = true,
}
local SQUAD_STATE: { [string]: boolean } = {
    forming = true, garrisoned = true, ordered = true, disbanded = true,
}
local ORDERS: { [string]: boolean } = {
    follow = true, move = true, hold = true, patrol = true,
    garrison = true, attack = true, retreat = true,
}
local FORMATIONS: { [string]: boolean } = { line = true, column = true, wedge = true, square = true }
local PAUSE: { [string]: boolean } = {
    unloaded = true, no_workers = true, missing_input = true,
    blocked_route = true, interrupted = true,
}

-- C4: every physical work order and squad order is a closed core union. The
-- plugin owns which bounded target and which real inputs to name; core executes.
-- `slot` is 0 main hand, 1 off hand, 2 head, 3 chest, 4 legs, 5 feet (equipment)
-- or 0-7 (carry). A role's kit is drawn from the employer's real inventory only.
local ROLE_KIT: any = {
    militia = {
        { endpoint = "equipment", slot = 0, item = "minecraft:iron_sword", count = 1 },
        { endpoint = "equipment", slot = 3, item = "minecraft:leather_chestplate", count = 1 },
    },
    infantry = {
        { endpoint = "equipment", slot = 0, item = "minecraft:iron_sword", count = 1 },
        { endpoint = "equipment", slot = 1, item = "minecraft:shield", count = 1 },
        { endpoint = "equipment", slot = 3, item = "minecraft:iron_chestplate", count = 1 },
    },
    spearman = {
        { endpoint = "equipment", slot = 0, item = "minecraft:trident", count = 1 },
        { endpoint = "equipment", slot = 3, item = "minecraft:chainmail_chestplate", count = 1 },
    },
    archer = {
        { endpoint = "equipment", slot = 0, item = "minecraft:bow", count = 1 },
        { endpoint = "equipment", slot = 3, item = "minecraft:leather_chestplate", count = 1 },
        { endpoint = "carry", slot = 0, item = "minecraft:arrow", count = 32 },
    },
}

-- One physical work order per civilian job, bound to the committed workplace
-- the settlement already owns. `units` is the bounded work budget of one call.
local JOB_WORK: any = {
    farming = { kind = "harvest", shop = "solaris:farm", tool = "minecraft:hoe", units = 64, y_min = -1, y_max = 2 },
    forestry = { kind = "cut_tree", shop = "solaris:sawmill", tool = "minecraft:axe", units = 64, y_min = 0, y_max = 15 },
    mining = { kind = "mine", shop = "solaris:mine_entrance", tool = "minecraft:pickaxe", units = 64, y_min = -12, y_max = 3 },
    fishing = { kind = "fish", shop = "solaris:fishing_pier", tool = "minecraft:fishing_rod", units = 32, y_min = -2, y_max = 1 },
    ranching = { kind = "tend_livestock", shop = "solaris:pen", feed = "minecraft:wheat", units = 16, y_min = -1, y_max = 2 },
    construction = { kind = "construct", units = 256 },
    hauling = { kind = "haul", units = 64 },
    crafting = { kind = "craft", recipe = "minecraft:stick", units = 4 },
}

local SQUAD_FORMATION_SPACING = 4
local SQUAD_ENGAGEMENT_RADIUS = 16
local SQUAD_POLICY_REVISION = 1
local MAX_SQUAD_TARGETS = 8
local MAX_GEAR_BYTES = 32
local BUILDING_STATES: { [string]: boolean } = {
    projected = true, funded = true, building = true, paused = true,
    committed = true, cancelled = true,
}
local ACTIVE_BUILDING: { [string]: boolean } = {
    projected = true, funded = true, building = true, paused = true,
}
local LIFECYCLE: { [string]: boolean } = {
    alive_loaded = true, alive_unloaded = true, dead = true, released = true,
}
local PURPOSE: { [string]: boolean } = {
    settlement = true, expansion = true, restoration = true,
}

-- Authored blueprint catalog shipped as `structures/<name>.toml`. `role` is the
-- deterministic layout role, `tier` is the growth tier the building serves.
local CATALOG: any = {
    ["solaris:house_small"] = { role = "home", tier = "core" },
    ["solaris:house_large"] = { role = "home", tier = "core" },
    ["solaris:plaza_well"] = { role = "meeting", tier = "core" },
    ["solaris:market"] = { role = "work", tier = "civic" },
    ["solaris:warehouse"] = { role = "work", tier = "civic" },
    ["solaris:farm"] = { role = "work", tier = "core" },
    ["solaris:sawmill"] = { role = "work", tier = "core" },
    ["solaris:pen"] = { role = "work", tier = "core" },
    ["solaris:fishing_pier"] = { role = "work", tier = "core" },
    ["solaris:mine_entrance"] = { role = "work", tier = "core" },
    ["solaris:smithy"] = { role = "work", tier = "civic" },
    ["solaris:barracks"] = { role = "guard", tier = "military" },
    ["solaris:watch_post"] = { role = "guard", tier = "core" },
    ["solaris:watchtower"] = { role = "guard", tier = "military" },
    ["solaris:palisade_gate"] = { role = "guard", tier = "military" },
    ["solaris:stone_wall"] = { role = "guard", tier = "military" },
    ["solaris:stone_tower"] = { role = "guard", tier = "military" },
    ["solaris:manor_hall"] = { role = "meeting", tier = "estate" },
    ["solaris:keep"] = { role = "guard", tier = "fortress" },
    ["solaris:town_hall"] = { role = "meeting", tier = "town" },
    ["solaris:library"] = { role = "work", tier = "town" },
}

-- Growth gates. A gate opens only on committed buildings, living residents,
-- assigned work, or a verified supply projection. A name or a payment never
-- opens one.
local GATES: any = {
    village = { houses = 9, pop = 24, jobs = 12, food = 64, meeting = 1 },
    developed = { houses = 16, pop = 40, jobs = 24, food = 128, meeting = 1, market = 1 },
    estate1 = { pop = 12, food = 64, hall = 1 },
    estate2 = { pop = 16, food = 96, money = 64, library = 1, warehouse = 1 },
    fortress1 = { pop = 12, food = 64, barracks = 1, wall = 1, weapons = 8, garrison = 8 },
    fortress2 = { pop = 16, food = 96, money = 128, keep = 1, tower = 2, weapons = 16, garrison = 16 },
    town1 = { pop = 16, food = 96, market = 1, town_hall = 1, money = 64 },
    town2 = { pop = 24, food = 128, money = 128, library = 1, warehouse = 1 },
}

local MAX_NAME_BYTES = 16
local MAX_FAMILY_BYTES = 16
local MAX_SQUAD_MEMBERS = 16
local MAX_POI_ENTRIES = 64
local MAX_ROLES = 8
local ENCODED_LIMIT = 4000
local MAX_REQUESTS = 8
local MAX_RECOVERY_ATTEMPTS = 3
local MAX_DELETE_BATCH = 16
local STREET_ROUTE_LIMIT = 30

local INDEX_KEY = "settlements-index-v1"
local DONE = "-"

-- The Loader bundle `client/settlements-ui.zip` declares exactly one screen, and
-- this id is byte-identical to its index entry: the Loader drops an open whose
-- view id is not declared, and core refuses a view id that is not owned by this
-- plugin. The three action ids are the `action_button` widgets of that screen,
-- so the client only ever sends one of them and only while the presented model
-- declares it enabled.
local VIEW_ID = "solaris-settlements:overview"
local VIEW_REFRESH = "solaris-settlements:refresh"
local VIEW_NEXT = "solaris-settlements:page_next"
local VIEW_PREV = "solaris-settlements:page_prev"
-- The core model carries at most 64 rows, so the roster is walked 16 rows at a
-- time and the warehouse stock page is bounded separately. One page is one
-- kind of row: a roster slice or the stock page, never a mixture.
local VIEW_PAGE_ROWS = 16
local VIEW_MAX_STOCK = 64
local VIEW_TEXT_BYTES = 200
local VIEW_CELL_BYTES = 250
local VIEW_DIGITS = "0123456789"
-- Live client surfaces the plugin keeps bookkeeping for; the oldest is dropped
-- once this many are live, and the dropped instance answers nothing.
local MAX_VIEW_SESSIONS = 8

local MISSING_CORE_CALLS: { string } = {
    "package structures/*.toml discovery + feudal_settlements selector (C2) — runtime_unavailable",
    "cross-plugin money authority — treasury moves only by verified transfer",
}

-- ---------------------------------------------------------------------------
-- Small helpers
-- ---------------------------------------------------------------------------

S.split = function(text: string, separator: string): { string }
    local parts: { string } = {}
    local start = 1
    while true do
        local index = string.find(text, separator, start, true)
        if index == nil then
            parts[#parts + 1] = string.sub(text, start)
            return parts
        end
        parts[#parts + 1] = string.sub(text, start, index - 1)
        start = index + #separator
    end
end

S.valid_name = function(value: string): boolean
    return #value >= 2 and #value <= MAX_NAME_BYTES and string.match(value, "^[a-z0-9_-]+$") ~= nil
end

S.normalize_uuid = function(value: string): string?
    local normalized = string.lower((string.gsub(value, "-", "")))
    if #normalized ~= 32 or string.match(normalized, "^[0-9a-f]+$") == nil then return nil end
    return normalized
end

S.valid_opaque = function(value: string): boolean
    return #value >= 1 and #value <= 64 and string.find(value, "|", 1, true) == nil
end

-- A durable id must stay inside the core's id contract (`[a-z0-9_-]`, at most
-- MAX_SCRIPT_ID_BYTES = 64): only characters the core rejects are replaced.
-- `-` and `_` are legal in a name and legal in a core id, so they survive.
-- Replacing `-` with `_` collapsed accepted names such as `a-b` and `a_b` onto
-- one durable create identity, and the second create was then refused with
-- `operation_conflict`. There is no truncation: every caller's input is already
-- bounded by MAX_NAME_BYTES and the fixed key prefixes, and truncating here
-- could alias two distinct names.
S.sanitize_id = function(value: string): string
    return (string.gsub(string.lower(value), "[^a-z0-9_-]", "_"))
end

S.sorted_keys = function(values: any): { string }
    local ids: { string } = {}
    for key in pairs(values) do ids[#ids + 1] = key end
    table.sort(ids)
    return ids
end

S.count_keys = function(values: any): number
    local count = 0
    for _ in pairs(values) do count = count + 1 end
    return count
end

S.encode_fields = function(fields: { any }): string
    local parts: { string } = {}
    for index = 1, #fields do
        local value = fields[index]
        if value == nil then
            parts[index] = DONE
        elseif type(value) == "number" then
            parts[index] = tostring(value)
        else
            parts[index] = value
        end
    end
    return table.concat(parts, "|")
end

S.decode_fields = function(value: any, count: number): { string }?
    if type(value) ~= "string" then return nil end
    local parts = S.split(value, "|")
    if #parts ~= count then return nil end
    return parts
end

S.integer_field = function(text: any, minimum: number, maximum: number): number?
    if type(text) ~= "string" then return nil end
    local value = tonumber(text)
    if value == nil or value % 1 ~= 0 or value < minimum or value > maximum then return nil end
    return value
end

S.resource_id = function(text: string): boolean
    return #text <= 64 and string.match(text, "^[a-z0-9_.-]+:[a-z0-9_./-]+$") ~= nil
end

S.contains = function(values: { string }, needle: string): boolean
    for index = 1, #values do
        if values[index] == needle then return true end
    end
    return false
end

-- Player command snapshots carry no dimension in API 0.6.0, so every
-- settlement operation maps to the operator-configured dimension.
local DIMENSION: string = raw_config.dimension
assert(type(DIMENSION) == "string" and S.resource_id(DIMENSION), "dimension must be a namespaced resource id")

-- ---------------------------------------------------------------------------
-- Storage keys and durable state
-- ---------------------------------------------------------------------------

S.settlement_key = function(id: string): string return "settlement:" .. id end
S.building_index_key = function(id: string): string return "bidx:" .. id end
S.resident_index_key = function(id: string): string return "ridx:" .. id end
S.squad_index_key = function(id: string): string return "sidx:" .. id end
S.operations_key = function(id: string): string return "ops:" .. id end
S.survey_key = function(id: string): string return "survey:" .. id end
S.site_key = function(id: string): string return "site:" .. id end
S.building_key = function(id: string, name: string): string return "building:" .. id .. ":" .. name end
S.plan_key = function(id: string, name: string): string return "plan:" .. id .. ":" .. name end
S.resident_key = function(id: string, name: string): string return "resident:" .. id .. ":" .. name end
S.squad_key = function(id: string, name: string): string return "squad:" .. id .. ":" .. name end

local settlements: any = {}
local building_ids: any = {}
local resident_ids: any = {}
local squad_ids: any = {}
local operation_ids: any = {}
local surveys: any = {}
local sites: any = {}
local missing: any = {}
local settlements_index: { string } = {}

local records: any = {}
local record_versions: any = {}

local requests: any = {}
local request_serial = 0
local cycle_timers: any = {}

-- Live client sessions, both bounded by MAX_VIEW_SESSIONS. `view_sessions`
-- answers an action by view instance id; `view_opens` binds an in-flight read
-- to its session by the session's own open request id, which exists from the
-- moment the open leaves the plugin. Both hold the same session table.
local view_sessions: any = {}
local view_opens: any = {}
local view_order: { any } = {}

local loaded_ids: { string } = {}
local boot_done = false

S.version_of = function(key: string): any return record_versions[key] end
S.set_version = function(key: string, version: any) record_versions[key] = version end

-- ---------------------------------------------------------------------------
-- Record codecs
-- ---------------------------------------------------------------------------

local SETTLEMENT_FIELDS = 30

S.write_settlement = function(record: any): string
    local roles = record.roles
    local role_ids = S.sorted_keys(roles)
    local role_text = DONE
    if #role_ids > 0 then
        local rows: { string } = {}
        for index = 1, #role_ids do
            rows[index] = role_ids[index] .. ":" .. roles[role_ids[index]]
        end
        role_text = table.concat(rows, "+")
    end
    return S.encode_fields({
        "v2",
        record.name,
        record.owner,
        record.size_class,
        record.stage,
        record.branch,
        record.branch_level,
        record.specs,
        record.condition,
        record.flags,
        record.pause,
        record.site_id,
        record.variant,
        record.site_revision,
        record.houses,
        record.meeting,
        record.pop,
        record.jobs,
        record.food,
        record.materials,
        record.metal,
        record.weapons,
        record.money,
        record.supply_tick,
        record.military,
        role_text,
        record.op,
        record.op_kind,
        record.revision,
        record.ticks,
    })
end

S.read_settlement = function(value: any): any?
    local parts = S.decode_fields(value, SETTLEMENT_FIELDS)
    if parts == nil or parts[1] ~= "v2" then return nil end
    local name = parts[2]
    local owner = S.normalize_uuid(parts[3])
    if not S.valid_name(name) or owner == nil then return nil end
    if SIZE_CLASS[parts[4]] ~= true or STAGE[parts[5]] ~= true or BRANCH[parts[6]] ~= true then return nil end
    local branch_level = S.integer_field(parts[7], 0, 2)
    if branch_level == nil then return nil end
    if parts[8] ~= DONE then
        local seen: any = {}
        local count = 0
        for _, spec in ipairs(S.split(parts[8], "+")) do
            if SPECS[spec] ~= true or seen[spec] ~= nil then return nil end
            seen[spec] = true
            count = count + 1
        end
        if count > 2 then return nil end
    end
    if CONDITION[parts[9]] ~= true then return nil end
    if parts[10] ~= DONE and parts[10] ~= "replan" then return nil end
    if parts[11] ~= DONE and PAUSE[parts[11]] ~= true then return nil end
    if parts[12] ~= DONE and not S.valid_opaque(parts[12]) then return nil end
    if parts[13] ~= DONE and STAGE[parts[13]] ~= true then return nil end
    local numeric: { number } = {}
    for offset = 0, 11 do
        local value_number = S.integer_field(parts[14 + offset], 0, 9007199254740991)
        if value_number == nil then return nil end
        numeric[offset + 1] = value_number
    end
    local roles: any = {}
    if parts[26] ~= DONE then
        local entries = S.split(parts[26], "+")
        if #entries > MAX_ROLES then return nil end
        for index = 1, #entries do
            local uuid_text, role = string.match(entries[index], "^([0-9a-f]+):([a-z]+)$")
            local uuid = S.normalize_uuid(uuid_text or "")
            if uuid == nil or roles[uuid] ~= nil then return nil end
            if role ~= "owner" and role ~= "steward" and role ~= "captain" then return nil end
            roles[uuid] = role
        end
    end
    if roles[owner] ~= "owner" then return nil end
    if parts[27] ~= DONE and not S.valid_opaque(parts[27]) then return nil end
    if parts[28] ~= DONE and not S.valid_opaque(parts[28]) then return nil end
    if (parts[27] == DONE) ~= (parts[28] == DONE) then return nil end
    local revision = S.integer_field(parts[29], 0, 9007199254740991)
    local ticks = S.integer_field(parts[30], 0, 9007199254740991)
    if revision == nil or ticks == nil then return nil end
    return {
        name = name,
        owner = owner,
        size_class = parts[4],
        stage = parts[5],
        branch = parts[6],
        branch_level = branch_level,
        specs = parts[8],
        condition = parts[9],
        flags = parts[10],
        pause = parts[11],
        site_id = parts[12],
        variant = parts[13],
        site_revision = numeric[1],
        houses = numeric[2],
        meeting = numeric[3],
        pop = numeric[4],
        jobs = numeric[5],
        food = numeric[6],
        materials = numeric[7],
        metal = numeric[8],
        weapons = numeric[9],
        money = numeric[10],
        supply_tick = numeric[11],
        military = numeric[12],
        roles = roles,
        op = parts[27],
        op_kind = parts[28],
        revision = revision,
        ticks = ticks,
    }
end

S.bump = function(record: any): string
    record.revision = record.revision + 1
    return tostring(record.revision)
end

local BUILDING_FIELDS = 19

S.write_building = function(record: any): string
    return S.encode_fields({
        "v1" :: any,
        record.name,
        record.blueprint,
        record.state,
        record.structure_id,
        record.rotation,
        record.ox,
        record.oy,
        record.oz,
        record.stage_index,
        record.built,
        record.watermark,
        record.reservation,
        record.plan_hash,
        record.structure_revision,
        record.op,
        record.op_kind,
        record.actor,
        record.revision,
    })
end

S.read_building = function(value: any): any?
    local parts = S.decode_fields(value, BUILDING_FIELDS)
    if parts == nil or parts[1] ~= "v1" then return nil end
    if not S.valid_name(parts[2]) or CATALOG[parts[3]] == nil then return nil end
    if BUILDING_STATES[parts[4]] ~= true then return nil end
    if parts[5] ~= DONE and not S.valid_opaque(parts[5]) then return nil end
    local rotation = S.integer_field(parts[6], 0, 270)
    if rotation == nil or (rotation % 90) ~= 0 then return nil end
    local numbers: { number } = {}
    local offsets = { 7, 8, 9, 10, 11, 12, 15, 18, 19 }
    for index = 1, #offsets do
        local value_number = S.integer_field(parts[offsets[index]], 0, 9007199254740991)
        if value_number == nil then return nil end
        numbers[index] = value_number
    end
    if parts[13] ~= DONE and not S.valid_opaque(parts[13]) then return nil end
    if parts[14] ~= DONE and string.match(parts[14], "^[0-9a-f]+$") == nil then return nil end
    if parts[16] ~= DONE and not S.valid_opaque(parts[16]) then return nil end
    if parts[17] ~= DONE and not S.valid_opaque(parts[17]) then return nil end
    if (parts[16] == DONE) ~= (parts[17] == DONE) then return nil end
    if parts[4] == "projected" and parts[5] == DONE then return nil end
    return {
        name = parts[2],
        blueprint = parts[3],
        role = CATALOG[parts[3]].role,
        state = parts[4],
        structure_id = parts[5],
        rotation = rotation,
        ox = numbers[1],
        oy = numbers[2],
        oz = numbers[3],
        stage_index = numbers[4],
        built = numbers[5],
        watermark = numbers[6],
        structure_revision = numbers[7],
        actor = numbers[8],
        revision = numbers[9],
        reservation = parts[13],
        plan_hash = parts[14],
        op = parts[16],
        op_kind = parts[17],
    }
end

local RESIDENT_FIELDS = 18

S.write_resident = function(record: any): string
    return S.encode_fields({
        "v1" :: any,
        record.name,
        record.handle,
        record.generation,
        record.entity_uuid,
        record.family,
        record.job,
        record.service,
        record.role,
        record.squad,
        record.house,
        record.home_poi,
        record.work_poi,
        record.life,
        record.op,
        record.op_kind,
        record.actor,
        record.revision,
    })
end

S.read_resident = function(value: any): any?
    local parts = S.decode_fields(value, RESIDENT_FIELDS)
    if parts == nil or parts[1] ~= "v1" then return nil end
    if not S.valid_name(parts[2]) then return nil end
    if parts[3] ~= DONE and not S.valid_opaque(parts[3]) then return nil end
    if parts[4] ~= DONE and not S.valid_opaque(parts[4]) then return nil end
    if parts[5] ~= DONE and S.normalize_uuid(parts[5]) == nil then return nil end
    if parts[6] ~= DONE and not S.valid_name(parts[6]) then return nil end
    if parts[7] ~= DONE and JOBS[parts[7]] ~= true then return nil end
    if SERVICE[parts[8]] ~= true then return nil end
    if parts[9] ~= DONE and MILITARY_ROLES[parts[9]] ~= true then return nil end
    if parts[10] ~= DONE and not S.valid_name(parts[10]) then return nil end
    -- `house` references the building/blueprint the resident occupies.
    if parts[11] ~= DONE and not S.valid_opaque(parts[11]) then return nil end
    if parts[12] ~= DONE and not S.valid_opaque(parts[12]) then return nil end
    if parts[13] ~= DONE and not S.valid_opaque(parts[13]) then return nil end
    if LIFECYCLE[parts[14]] ~= true then return nil end
    if parts[15] ~= DONE and not S.valid_opaque(parts[15]) then return nil end
    if parts[16] ~= DONE and not S.valid_opaque(parts[16]) then return nil end
    if (parts[15] == DONE) ~= (parts[16] == DONE) then return nil end
    local revision = S.integer_field(parts[18], 0, 9007199254740991)
    if revision == nil then return nil end
    local service = parts[8]
    local job = parts[7]
    local role = parts[9]
    if service == "civilian" and role ~= DONE then return nil end
    if (service == "military" or service == "recruiting") and role == DONE then return nil end
    if job ~= DONE and role ~= DONE then return nil end
    return {
        name = parts[2],
        handle = parts[3],
        generation = parts[4],
        entity_uuid = parts[5],
        family = parts[6],
        job = job,
        service = service,
        role = role,
        squad = parts[10],
        house = parts[11],
        home_poi = parts[12],
        work_poi = parts[13],
        life = parts[14],
        op = parts[15],
        op_kind = parts[16],
        actor = parts[17] == DONE and 0 or (tonumber(parts[17]) or 0),
        revision = revision,
    }
end

local SQUAD_FIELDS = 10

S.write_squad = function(record: any): string
    local roster: { string } = {}
    for index = 1, #record.roster do
        local member = record.roster[index]
        roster[index] = member.name .. ":" .. member.handle .. ":" .. tostring(member.order_revision)
    end
    local targets: { string } = {}
    for index = 1, #record.targets do targets[index] = record.targets[index] end
    return S.encode_fields({
        "v2" :: any,
        record.name,
        record.role,
        record.formation,
        record.state,
        record.order,
        #roster == 0 and DONE or table.concat(roster, "+"),
        record.post,
        #targets == 0 and DONE or table.concat(targets, "+"),
        record.revision,
    })
end

S.read_squad = function(value: any): any?
    if type(value) ~= "string" then return nil end
    local parts = S.split(value, "|")
    if parts[1] == "v1" then
        if #parts ~= 9 then return nil end
        if not S.valid_name(parts[2]) then return nil end
        if parts[3] ~= DONE and MILITARY_ROLES[parts[3]] ~= true then return nil end
        if FORMATIONS[parts[4]] ~= true or SQUAD_STATE[parts[5]] ~= true then return nil end
        if parts[6] ~= DONE and ORDERS[parts[6]] ~= true then return nil end
        local roster: { any } = {}
        if parts[7] ~= DONE then
            local entries = S.split(parts[7], "+")
            if #entries > MAX_SQUAD_MEMBERS then return nil end
            for index = 1, #entries do
                if not S.valid_name(entries[index]) then return nil end
                roster[index] = { name = entries[index], handle = DONE, order_revision = 0 }
            end
        end
        if parts[8] ~= DONE and not S.valid_opaque(parts[8]) then return nil end
        local revision = S.integer_field(parts[9], 0, 9007199254740991)
        if revision == nil then return nil end
        local members: { string } = {}
        for index = 1, #roster do members[index] = roster[index].name end
        return {
            name = parts[2], role = parts[3], formation = parts[4], state = parts[5],
            order = parts[6], roster = roster, members = members, targets = {},
            post = parts[8], revision = revision,
        }
    end
    if #parts ~= SQUAD_FIELDS or parts[1] ~= "v2" then return nil end
    if not S.valid_name(parts[2]) then return nil end
    if parts[3] ~= DONE and MILITARY_ROLES[parts[3]] ~= true then return nil end
    if FORMATIONS[parts[4]] ~= true or SQUAD_STATE[parts[5]] ~= true then return nil end
    if parts[6] ~= DONE and ORDERS[parts[6]] ~= true then return nil end
    local roster: { any } = {}
    local members: { string } = {}
    if parts[7] ~= DONE then
        local entries = S.split(parts[7], "+")
        if #entries > MAX_SQUAD_MEMBERS then return nil end
        for index = 1, #entries do
            local name, handle, revision_text = string.match(entries[index], "^([a-z0-9_-]+):([^:]+):([0-9]+)$")
            if name == nil or not S.valid_name(name) then return nil end
            if handle ~= DONE and not S.valid_opaque(handle) then return nil end
            local order_revision = S.integer_field(revision_text, 0, 9007199254740991)
            if order_revision == nil then return nil end
            roster[index] = { name = name, handle = handle, order_revision = order_revision }
            members[index] = name
        end
    end
    if parts[8] ~= DONE and not S.valid_opaque(parts[8]) then return nil end
    local targets: { string } = {}
    if parts[9] ~= DONE then
        local entries = S.split(parts[9], "+")
        if #entries > MAX_SQUAD_TARGETS then return nil end
        for index = 1, #entries do
            if not S.valid_opaque(entries[index]) then return nil end
            targets[index] = entries[index]
        end
    end
    local revision = S.integer_field(parts[10], 0, 9007199254740991)
    if revision == nil then return nil end
    return {
        name = parts[2], role = parts[3], formation = parts[4], state = parts[5],
        order = parts[6], roster = roster, members = members, targets = targets,
        post = parts[8], revision = revision,
    }
end

local SURVEY_FIELDS = 19

S.write_survey = function(record: any): string
    return S.encode_fields({
        "v1" :: any,
        record.token,
        record.revision,
        record.dimension,
        record.min_x, record.min_y, record.min_z,
        record.max_x, record.max_y, record.max_z,
        record.usable, record.water,
        record.claimed and 1 or 0,
        record.existing,
        record.biome_tags,
        record.resource_tags,
        record.purpose,
        record.tick,
        record.plugin_revision,
    })
end

S.read_survey = function(value: any): any?
    local parts = S.decode_fields(value, SURVEY_FIELDS)
    if parts == nil or parts[1] ~= "v1" then return nil end
    if not S.valid_opaque(parts[2]) then return nil end
    local revision = S.integer_field(parts[3], 0, 9007199254740991)
    if revision == nil or not S.resource_id(parts[4]) then return nil end
    local coordinates: { number } = {}
    for index = 1, 6 do
        local value_number = S.integer_field(parts[4 + index], -30000000, 9007199254740991)
        if value_number == nil then return nil end
        coordinates[index] = value_number
    end
    if coordinates[1] > coordinates[4] or coordinates[2] > coordinates[5] or coordinates[3] > coordinates[6] then
        return nil
    end
    if coordinates[4] - coordinates[1] + 1 > 128 or coordinates[6] - coordinates[3] + 1 > 128 then return nil end
    local usable = S.integer_field(parts[11], 0, 9007199254740991)
    local water = S.integer_field(parts[12], 0, 9007199254740991)
    local existing = S.integer_field(parts[14], 0, 9007199254740991)
    if usable == nil or water == nil or existing == nil then return nil end
    if parts[13] ~= "0" and parts[13] ~= "1" then return nil end
    if PURPOSE[parts[17]] ~= true then return nil end
    local tick = S.integer_field(parts[18], 0, 9007199254740991)
    local plugin_revision = S.integer_field(parts[19], 0, 9007199254740991)
    if tick == nil or plugin_revision == nil then return nil end
    return {
        token = parts[2],
        revision = revision,
        dimension = parts[4],
        min_x = coordinates[1], min_y = coordinates[2], min_z = coordinates[3],
        max_x = coordinates[4], max_y = coordinates[5], max_z = coordinates[6],
        usable = usable,
        water = water,
        claimed = parts[13] == "1",
        existing = existing,
        biome_tags = parts[15],
        resource_tags = parts[16],
        purpose = parts[17],
        tick = tick,
        plugin_revision = plugin_revision,
    }
end

S.site_fields = function(count: number): number
    return 11 + count * 5
end

S.write_site = function(record: any): string
    local fields: { any } = {
        "v1", record.site_id, record.variant, record.revision,
        record.min_x, record.min_y, record.min_z,
        record.size_x, record.size_y, record.size_z,
        #record.pois,
    }
    for index = 1, #record.pois do
        local poi = record.pois[index]
        fields[#fields + 1] = poi.poi_id
        fields[#fields + 1] = poi.kind
        fields[#fields + 1] = poi.capacity
        fields[#fields + 1] = poi.state
        fields[#fields + 1] = poi.blueprint
    end
    return S.encode_fields(fields)
end

S.read_site = function(value: any): any?
    if type(value) ~= "string" then return nil end
    local parts = S.split(value, "|")
    if #parts < 11 or parts[1] ~= "v1" then return nil end
    if not S.valid_opaque(parts[2]) or STAGE[parts[3]] ~= true then return nil end
    local revision = S.integer_field(parts[4], 0, 9007199254740991)
    local count = S.integer_field(parts[11], 0, MAX_POI_ENTRIES)
    if revision == nil or count == nil then return nil end
    if #parts ~= S.site_fields(count) then return nil end
    local numbers: { number } = {}
    for index = 1, 6 do
        local value_number = S.integer_field(parts[4 + index], -30000000, 9007199254740991)
        if value_number == nil then return nil end
        numbers[index] = value_number
    end
    local pois: any = {}
    for index = 1, count do
        local base = 12 + (index - 1) * 5
        if not S.valid_opaque(parts[base]) or POI_KIND[parts[base + 1]] ~= true then return nil end
        local capacity = S.integer_field(parts[base + 2], 0, 65535)
        if capacity == nil or POI_STATE[parts[base + 3]] ~= true then return nil end
        if CATALOG[parts[base + 4]] == nil then return nil end
        pois[index] = {
            poi_id = parts[base],
            kind = parts[base + 1],
            capacity = capacity,
            state = parts[base + 3],
            blueprint = parts[base + 4],
        }
    end
    return {
        site_id = parts[2],
        variant = parts[3],
        revision = revision,
        min_x = numbers[1], min_y = numbers[2], min_z = numbers[3],
        size_x = numbers[4], size_y = numbers[5], size_z = numbers[6],
        pois = pois,
    }
end

S.write_plan = function(record: any): string
    local fields: { any } = { "v1", record.reservation_ref, record.plan_hash, record.revision, #record.order }
    for index = 1, #record.order do fields[#fields + 1] = record.order[index] end
    for index = 1, #record.order do
        local stage = record.stages[record.order[index]]
        fields[#fields + 1] = stage.units
        fields[#fields + 1] = stage.materials
    end
    return S.encode_fields(fields)
end

S.read_plan = function(value: any): any?
    if type(value) ~= "string" then return nil end
    local parts = S.split(value, "|")
    if #parts < 8 or parts[1] ~= "v1" then return nil end
    if parts[2] ~= DONE and not S.valid_opaque(parts[2]) then return nil end
    if string.match(parts[3], "^[0-9a-f]+$") == nil then return nil end
    local revision = S.integer_field(parts[4], 0, 9007199254740991)
    local count = S.integer_field(parts[5], 1, 32)
    if revision == nil or count == nil then return nil end
    if #parts ~= 5 + count * 3 then return nil end
    local order: { string } = {}
    local stages: any = {}
    for index = 1, count do
        local name = parts[5 + index]
        if not S.valid_opaque(name) or stages[name] ~= nil then return nil end
        order[index] = name
        stages[name] = { units = 0, materials = DONE }
    end
    for index = 1, count do
        local base = 5 + count + (index - 1) * 2
        local units = S.integer_field(parts[base + 1], 1, 9007199254740991)
        if units == nil then return nil end
        stages[order[index]].units = units
        stages[order[index]].materials = parts[base + 2]
    end
    return {
        reservation_ref = parts[2],
        plan_hash = parts[3],
        revision = revision,
        order = order,
        stages = stages,
    }
end

S.material_pairs = function(text: string): any
    if text == DONE then return {} end
    local pairs_out: { any } = {}
    for _, entry in ipairs(S.split(text, ";")) do
        local resource, count = string.match(entry, "^([a-z0-9_.-]+:[a-z0-9_./-]+)=(%d+)$")
        local quantity = tonumber(count or "")
        if resource == nil or quantity == nil or quantity <= 0 then return nil end
        local pair: any = { resource = resource, quantity = quantity }
        pairs_out[#pairs_out + 1] = pair
    end
    return pairs_out
end

S.plan_to_resource_plan = function(plan: any): any
    local portions: any = {}
    for index = 1, #plan.order do
        local stage = plan.stages[plan.order[index]]
        local pairs_out = S.material_pairs(stage.materials) or {}
        table.sort(pairs_out, function(left: any, right: any) return left.resource < right.resource end)
        local materials: any = {}
        for position = 1, #pairs_out do
            materials[position] = { resource = pairs_out[position].resource, quantity = pairs_out[position].quantity }
        end
        portions[index] = { work_units = stage.units, materials = materials }
    end
    return { portions = portions }
end

S.stage_bounds = function(plan: any, stage_index: number): any
    local prior = 0
    for index = 1, stage_index do
        prior = prior + plan.stages[plan.order[index]].units
    end
    return { prior = prior, units = plan.stages[plan.order[stage_index + 1]].units }
end

-- ---------------------------------------------------------------------------
-- Index records (derived, written in the same atomic batch as their records)
-- ---------------------------------------------------------------------------

S.index_add = function(index: any, entry: any): boolean
    for position = 1, #index do
        if index[position].name == entry.name then return false end
    end
    index[#index + 1] = entry
    table.sort(index, function(left: any, right: any) return left.name < right.name end)
    return true
end

S.index_remove = function(index: any, name: string): boolean
    for position = 1, #index do
        if index[position].name == name then
            table.remove(index, position)
            return true
        end
    end
    return false
end

S.index_entry = function(index: any, name: string): any
    for position = 1, #index do
        if index[position].name == name then return index[position] end
    end
    return nil
end

-- The durable operations index is keyed by `target` (the field the encoder
-- writes), not by the `name` field the building/resident/squad indexes use.
-- Looking it up as a `name` never matched, so every intent appended a fresh
-- entry until the cap and nothing was ever cleared.
S.pending_entry = function(index: any, target: string): any
    for position = 1, #index do
        if index[position].target == target then return index[position] end
    end
    return nil
end

S.encode_building_index = function(id: string, omit: string?): string?
    local index = building_ids[id] or {}
    local fields: { any } = { "v1" }
    for position = 1, #index do
        local entry = index[position]
        if entry.name ~= omit then
            fields[#fields + 1] = entry.name
            fields[#fields + 1] = entry.blueprint
            fields[#fields + 1] = entry.state
        end
    end
    local encoded = S.encode_fields(fields)
    if #encoded > ENCODED_LIMIT then return nil end
    return encoded
end

S.decode_building_index = function(value: any): any?
    if type(value) ~= "string" then return nil end
    local parts = S.split(value, "|")
    if parts[1] ~= "v1" then return nil end
    if (#parts - 1) % 3 ~= 0 then return nil end
    local index: any = {}
    for position = 1, (#parts - 1) / 3 do
        local base = 2 + (position - 1) * 3
        if not S.valid_name(parts[base]) or CATALOG[parts[base + 1]] == nil then return nil end
        if BUILDING_STATES[parts[base + 2]] ~= true then return nil end
        index[position] = { name = parts[base], blueprint = parts[base + 1], state = parts[base + 2] }
    end
    return index
end

S.encode_resident_index = function(id: string): string?
    local index = resident_ids[id] or {}
    local fields: { any } = { "v2" }
    for position = 1, #index do
        local entry = index[position]
        fields[#fields + 1] = entry.name
        fields[#fields + 1] = entry.family
        fields[#fields + 1] = entry.job
        fields[#fields + 1] = entry.service
        fields[#fields + 1] = entry.squad
        fields[#fields + 1] = entry.life
        fields[#fields + 1] = entry.role or DONE
        fields[#fields + 1] = entry.gear or DONE
    end
    local encoded = S.encode_fields(fields)
    if #encoded > ENCODED_LIMIT then return nil end
    return encoded
end

S.decode_resident_index = function(value: any): any?
    if type(value) ~= "string" then return nil end
    local parts = S.split(value, "|")
    if parts[1] == "v1" then
        if (#parts - 1) % 6 ~= 0 then return nil end
        local index: any = {}
        for position = 1, (#parts - 1) / 6 do
            local base = 2 + (position - 1) * 6
            if not S.valid_name(parts[base]) then return nil end
            if parts[base + 2] ~= DONE and JOBS[parts[base + 2]] ~= true then return nil end
            if SERVICE[parts[base + 3]] ~= true then return nil end
            if LIFECYCLE[parts[base + 5]] ~= true then return nil end
            index[position] = {
                name = parts[base],
                family = parts[base + 1],
                job = parts[base + 2],
                service = parts[base + 3],
                squad = parts[base + 4],
                life = parts[base + 5],
                role = DONE,
                gear = DONE,
            }
        end
        return index
    end
    if parts[1] ~= "v2" then return nil end
    if (#parts - 1) % 8 ~= 0 then return nil end
    local index: any = {}
    for position = 1, (#parts - 1) / 8 do
        local base = 2 + (position - 1) * 8
        if not S.valid_name(parts[base]) then return nil end
        if parts[base + 2] ~= DONE and JOBS[parts[base + 2]] ~= true then return nil end
        if SERVICE[parts[base + 3]] ~= true then return nil end
        if LIFECYCLE[parts[base + 5]] ~= true then return nil end
        if parts[base + 6] ~= DONE and MILITARY_ROLES[parts[base + 6]] ~= true then return nil end
        index[position] = {
            name = parts[base],
            family = parts[base + 1],
            job = parts[base + 2],
            service = parts[base + 3],
            squad = parts[base + 4],
            life = parts[base + 5],
            role = parts[base + 6],
            gear = parts[base + 7],
        }
    end
    return index
end

S.encode_squad_index = function(id: string): string?
    local index = squad_ids[id] or {}
    local fields: { any } = { "v2" }
    for position = 1, #index do
        local entry = index[position]
        fields[#fields + 1] = entry.name
        fields[#fields + 1] = entry.state
        fields[#fields + 1] = entry.order
        fields[#fields + 1] = entry.role
        fields[#fields + 1] = entry.members or 0
        fields[#fields + 1] = entry.armed or 0
    end
    local encoded = S.encode_fields(fields)
    if #encoded > ENCODED_LIMIT then return nil end
    return encoded
end

S.decode_squad_index = function(value: any): any?
    if type(value) ~= "string" then return nil end
    local parts = S.split(value, "|")
    if parts[1] == "v1" then
        if (#parts - 1) % 4 ~= 0 then return nil end
        local index: any = {}
        for position = 1, (#parts - 1) / 4 do
            local base = 2 + (position - 1) * 4
            if not S.valid_name(parts[base]) or SQUAD_STATE[parts[base + 1]] ~= true then return nil end
            if parts[base + 2] ~= DONE and ORDERS[parts[base + 2]] ~= true then return nil end
            index[position] = {
                name = parts[base], state = parts[base + 1], order = parts[base + 2],
                role = parts[base + 3], members = 0, armed = 0,
            }
        end
        return index
    end
    if parts[1] ~= "v2" then return nil end
    if (#parts - 1) % 6 ~= 0 then return nil end
    local index: any = {}
    for position = 1, (#parts - 1) / 6 do
        local base = 2 + (position - 1) * 6
        if not S.valid_name(parts[base]) or SQUAD_STATE[parts[base + 1]] ~= true then return nil end
        if parts[base + 2] ~= DONE and ORDERS[parts[base + 2]] ~= true then return nil end
        local members = S.integer_field(parts[base + 4], 0, MAX_SQUAD_MEMBERS)
        local armed = S.integer_field(parts[base + 5], 0, MAX_SQUAD_MEMBERS)
        if members == nil or armed == nil then return nil end
        index[position] = {
            name = parts[base], state = parts[base + 1], order = parts[base + 2],
            role = parts[base + 3], members = members, armed = armed,
        }
    end
    return index
end

S.encode_operations = function(id: string): string?
    local index = operation_ids[id] or {}
    local fields: { any } = { "v1" }
    for position = 1, #index do
        local entry = index[position]
        fields[#fields + 1] = entry.target
        fields[#fields + 1] = entry.operation_id
        fields[#fields + 1] = entry.kind
        fields[#fields + 1] = entry.attempts
        fields[#fields + 1] = entry.actor
        fields[#fields + 1] = entry.detail
    end
    local encoded = S.encode_fields(fields)
    if #encoded > ENCODED_LIMIT then return nil end
    return encoded
end

S.decode_operations = function(value: any): any?
    if type(value) ~= "string" then return nil end
    local parts = S.split(value, "|")
    if parts[1] ~= "v1" then return nil end
    if (#parts - 1) % 6 ~= 0 then return nil end
    local index: any = {}
    for position = 1, (#parts - 1) / 6 do
        local base = 2 + (position - 1) * 6
        local attempts = S.integer_field(parts[base + 3], 0, 99)
        if attempts == nil then return nil end
        index[position] = {
            target = parts[base],
            operation_id = parts[base + 1],
            kind = parts[base + 2],
            attempts = attempts,
            actor = tonumber(parts[base + 4]) or 0,
            detail = parts[base + 5],
        }
    end
    return index
end

S.encode_settlements_index = function(): string
    local fields: { any } = { "v1" }
    for index = 1, #settlements_index do fields[#fields + 1] = settlements_index[index] end
    return S.encode_fields(fields)
end

S.decode_settlements_index = function(value: any): { string }?
    if type(value) ~= "string" then return nil end
    local parts = S.split(value, "|")
    if parts[1] ~= "v1" then return nil end
    if #parts - 1 > MAX_SETTLEMENTS then return nil end
    local ids: { string } = {}
    for index = 2, #parts do
        if not S.valid_name(parts[index]) then return nil end
        ids[#ids + 1] = parts[index]
    end
    return ids
end

-- ---------------------------------------------------------------------------
-- Requests
-- ---------------------------------------------------------------------------

S.requests_pending = function(): number
    return S.count_keys(requests)
end

S.begin_request = function(kind: string, fields: any): any
    local entry: any = fields or {}
    if entry.request_id == nil then
        request_serial = request_serial + 1
        entry.request_id = "r" .. tostring(request_serial)
    end
    entry.kind = kind
    requests[entry.request_id] = entry
    return entry
end

S.finish_request = function(entry: any)
    requests[entry.request_id] = nil
end

S.read_key = function(key: string, kind: string, fields: any): boolean
    if S.requests_pending() >= MAX_REQUESTS then return false end
    local entry = S.begin_request(kind, fields)
    entry.key = key
    solaris.storage_get(entry.request_id, key)
    return true
end

-- Atomic multi-key write. `identity` names the write (primary key plus its next
-- revision), so a retry of the same intent reuses the same durable operation id
-- and replays, while a different write can never collide with it.
S.write_batch = function(mutations: { any }, purpose: string, fields: any, identity: string): boolean
    if S.requests_pending() >= MAX_REQUESTS then return false end
    if #mutations == 0 or #mutations > 16 then return false end
    local rows: { any } = {}
    local primary = ""
    for index = 1, #mutations do
        local mutation = mutations[index]
        local row: any = { key = mutation.key }
        if mutation.value == nil then
            row.operation = "delete"
        else
            row.operation = "cas"
            row.value = mutation.value
        end
        if mutation.version ~= nil then row.expected_version = mutation.version end
        rows[index] = row
        if primary == "" then primary = mutation.key end
    end
    local entry = S.begin_request("write", fields)
    entry.purpose = purpose
    entry.mutations = mutations
    solaris.storage_batch_cas(
        entry.request_id,
        "b-" .. S.sanitize_id(primary) .. "-" .. S.sanitize_id(identity),
        rows
    )
    return true
end

S.optional_mutation = function(key: string, value: string?): any
    return { key = key, version = S.version_of(key), value = value }
end

-- Persist the settlement record together with every derived index and any
-- value-record mutations the caller added, all in one atomic batch.
S.write_settlement_bundle = function(id: string, record: any, extras: { any }, purpose: string, fields: any): boolean
    record.revision = record.revision + 1
    local mutations: { any } = {}
    for index = 1, #extras do mutations[#mutations + 1] = extras[index] end
    local settlement_value = S.write_settlement(record)
    if #settlement_value > ENCODED_LIMIT then return false end
    table.insert(mutations, 1, {
        key = S.settlement_key(id),
        version = S.version_of(S.settlement_key(id)),
        value = settlement_value,
    })
    local building_index = S.encode_building_index(id)
    local resident_index = S.encode_resident_index(id)
    local squad_index = S.encode_squad_index(id)
    local operations_index = S.encode_operations(id)
    if building_index == nil or resident_index == nil or squad_index == nil or operations_index == nil then
        return false
    end
    mutations[#mutations + 1] = S.optional_mutation(S.building_index_key(id), building_index)
    mutations[#mutations + 1] = S.optional_mutation(S.resident_index_key(id), resident_index)
    mutations[#mutations + 1] = S.optional_mutation(S.squad_index_key(id), squad_index)
    mutations[#mutations + 1] = S.optional_mutation(S.operations_key(id), operations_index)
    if #mutations > 16 then return false end
    return S.write_batch(mutations, purpose, fields, tostring(record.revision))
end

-- ---------------------------------------------------------------------------
-- Read helpers
-- ---------------------------------------------------------------------------

S.message = function(player_id: any, text: string)
    if player_id == nil or player_id == 0 then return end
    solaris.send_message(player_id, text)
end

S.role_of = function(record: any, uuid: string): string?
    return record.roles[uuid]
end

S.can_build = function(record: any, uuid: string): boolean
    local role = record.roles[uuid]
    return role == "owner" or role == "steward"
end

S.can_lead = function(record: any, uuid: string): boolean
    local role = record.roles[uuid]
    return role == "owner" or role == "captain"
end

S.living_residents = function(id: string): number
    local index = resident_ids[id] or {}
    local count = 0
    for position = 1, #index do
        local life = index[position].life
        if life == "alive_loaded" or life == "alive_unloaded" then count = count + 1 end
    end
    return count
end

S.committed_blueprint = function(id: string, blueprint: string): number
    local index = building_ids[id] or {}
    local count = 0
    for position = 1, #index do
        if index[position].state == "committed" and index[position].blueprint == blueprint then
            count = count + 1
        end
    end
    return count
end

S.committed_role = function(id: string, role: string): number
    local index = building_ids[id] or {}
    local count = 0
    for position = 1, #index do
        local blueprint = index[position].blueprint
        if index[position].state == "committed" and CATALOG[blueprint] ~= nil and CATALOG[blueprint].role == role then
            count = count + 1
        end
    end
    return count
end

S.active_buildings = function(id: string): number
    local index = building_ids[id] or {}
    local count = 0
    for position = 1, #index do
        if ACTIVE_BUILDING[index[position].state] == true then count = count + 1 end
    end
    return count
end

S.refresh_counters = function(id: string)
    local record = settlements[id]
    if record == nil then return end
    record.houses = S.committed_role(id, "home")
    record.meeting = S.committed_role(id, "meeting")
    record.pop = S.living_residents(id)
    local jobs = 0
    local military = 0
    local index = resident_ids[id] or {}
    for position = 1, #index do
        local entry = index[position]
        if entry.life == "alive_loaded" or entry.life == "alive_unloaded" then
            if entry.job ~= DONE then jobs = jobs + 1 end
            if entry.service == "military" then military = military + 1 end
        end
    end
    record.jobs = jobs
    record.military = military
end

S.available_tier = function(record: any): string
    if record.branch == "fortress" and record.branch_level >= 2
        and S.committed_blueprint(record.name, "solaris:keep") >= 1 then
        return "castle"
    end
    if record.branch == "estate" and record.branch_level >= 2
        and S.committed_blueprint(record.name, "solaris:manor_hall") >= 1 then
        return "manor"
    end
    if record.branch == "town" and record.branch_level >= 2 then return "city" end
    if record.stage == "developed" then return "developed " .. record.branch end
    return record.stage
end

S.next_gate = function(record: any): (any?, string?)
    if record.stage == "hamlet" then return GATES.village, "village" end
    if record.stage == "village" then return GATES.developed, "developed" end
    if record.branch == "none" then return nil, nil end
    if record.branch_level == 0 then
        if record.branch == "estate" then return GATES.estate1, "estate level 1" end
        if record.branch == "fortress" then return GATES.fortress1, "fortress level 1" end
        return GATES.town1, "town level 1"
    end
    if record.branch_level == 1 then
        if record.branch == "estate" then return GATES.estate2, "estate level 2" end
        if record.branch == "fortress" then return GATES.fortress2, "fortress level 2" end
        return GATES.town2, "town level 2"
    end
    return nil, nil
end

S.missing_gate = function(record: any, gate: any): { string }
    local missing_requirements: { string } = {}
    local function need(label: string, have: number, wanted: number?)
        if wanted ~= nil and have < wanted then
            missing_requirements[#missing_requirements + 1] = label .. " " .. tostring(have) .. "/" .. tostring(wanted)
        end
    end
    need("houses", record.houses, gate.houses)
    need("residents", record.pop, gate.pop)
    need("jobs", record.jobs, gate.jobs)
    need("food", record.food, gate.food)
    need("money", record.money, gate.money)
    need("weapons", record.weapons, gate.weapons)
    need("soldiers", record.military, gate.garrison)
    local function need_building(label: string, blueprint: string, wanted: number?)
        if wanted ~= nil and S.committed_blueprint(record.name, blueprint) < wanted then
            missing_requirements[#missing_requirements + 1] = "committed " .. label
        end
    end
    need_building("meeting hall", "solaris:plaza_well", gate.meeting)
    need_building("market", "solaris:market", gate.market)
    need_building("manor hall", "solaris:manor_hall", gate.hall)
    need_building("barracks", "solaris:barracks", gate.barracks)
    need_building("stone wall", "solaris:stone_wall", gate.wall)
    need_building("towers", "solaris:stone_tower", gate.tower)
    need_building("keep", "solaris:keep", gate.keep)
    need_building("town hall", "solaris:town_hall", gate.town_hall)
    need_building("library", "solaris:library", gate.library)
    need_building("warehouse", "solaris:warehouse", gate.warehouse)
    return missing_requirements
end

S.is_member = function(record: any, uuid: string): boolean
    return record.roles[uuid] ~= nil
end

S.member_online = function(id: string, online: any): boolean
    local record = settlements[id]
    if record == nil then return false end
    for index = 1, #online do
        local player = online[index]
        local uuid = S.normalize_uuid(player.uuid or "")
        if uuid ~= nil and (record.roles[uuid] ~= nil or record.owner == uuid) then return true end
    end
    return false
end

S.near_settlement = function(id: string, online: any, radius: number): boolean
    local site = sites[id]
    if site == nil then return #online > 0 end
    for index = 1, #online do
        local player = online[index]
        local x = tonumber(player.x)
        local z = tonumber(player.z)
        if x ~= nil and z ~= nil then
            if x >= site.min_x - radius and x <= site.min_x + site.size_x + radius
                and z >= site.min_z - radius and z <= site.min_z + site.size_z + radius then
                return true
            end
        end
    end
    return false
end

-- ---------------------------------------------------------------------------
-- Core calls
-- ---------------------------------------------------------------------------

-- Every intent needs an operation id that is unique for its whole settlement
-- lifetime: core replays an id with the same fingerprint and returns
-- `operation_conflict` for a different one. The pending index is cleared as
-- intents commit, so its size cannot name an id; the settlement record's
-- monotonic `revision` can. Allocating an id consumes the next revision, and
-- the following bundle write persists it.
S.operation_id = function(id: string, kind: string): string
    local record = settlements[id]
    local serial = 1
    if record ~= nil then
        record.revision = record.revision + 1
        serial = record.revision
    end
    return kind .. "-" .. S.sanitize_id(id) .. "-" .. tostring(serial)
end

S.set_pending = function(id: string, target: string, op: string, kind: string, actor: number, detail: string)
    local index = operation_ids[id]
    if index == nil then
        index = {}
        operation_ids[id] = index
    end
    local entry = S.pending_entry(index, target)
    if entry == nil then
        if #index >= MAX_PENDING_OPS then return nil end
        entry = { attempts = 0 }
        index[#index + 1] = entry
    end
    entry.target = target
    entry.operation_id = op
    entry.kind = kind
    entry.actor = actor
    entry.detail = detail
    return entry
end

-- Drop a pending entry. A completion may only remove the entry it owns: the
-- optional `op` names the completion's operation id, so a stale completion
-- never erases a newer intent parked on the same target.
S.clear_pending = function(id: string, target: string, op: string?)
    local index = operation_ids[id]
    if index == nil then return end
    for position = 1, #index do
        local entry = index[position]
        if entry.target == target and (op == nil or entry.operation_id == op) then
            table.remove(index, position)
            return
        end
    end
end

S.pending_count = function(id: string): number
    return #(operation_ids[id] or {})
end

S.operation_target_of = function(id: string, target: string): any
    local index = operation_ids[id]
    if index == nil then return nil end
    return S.pending_entry(index, target)
end

-- A resident-site reservation is handed back only for the two refusals that
-- provably precede every effect of `spawn_resident`: `blocked` (no standable
-- site) and `unloaded` (chunks unknown). Every other answer still leaves a
-- spawned entity possible, so the reservation stays held and the durable
-- operation is re-queried: `runtime_unavailable` also collapses a failure that
-- happened after the owner entity was spawned, and `invalid_request` has the
-- same hole after handle minting; `not_found` is indistinguishable from an
-- absent receipt; `capacity` is the early branch that means the site token was
-- already consumed by a committed spawn; `busy` and `operation_conflict` never
-- prove the spawn did not commit.
S.spawn_refusal_is_pre_effect = function(failure: string?): boolean
    return failure == "blocked" or failure == "unloaded"
end

-- Ask the core for the durable receipt of the intent parked on `target`.
-- Bounded like the startup probes so an unresolved intent is re-queried a
-- fixed number of times and then left in place for the next recovery.
S.probe_pending = function(id: string, target: string, actor: number): boolean
    local intent = S.operation_target_of(id, target)
    if intent == nil or intent.attempts >= MAX_RECOVERY_ATTEMPTS then return false end
    if S.requests_pending() >= MAX_REQUESTS then return false end
    intent.attempts = intent.attempts + 1
    local probe = S.begin_request("recover", {
        id = id,
        target = target,
        operation_id = intent.operation_id,
        operation_kind = intent.kind,
        actor = actor,
        detail = intent.detail,
        recovery = true,
    })
    solaris.operation_status(probe.request_id, intent.operation_id)
    return true
end

-- Record a refused or failed game operation: drop the intent and tell the
-- actor. Nothing else changes, so a retry starts from the same durable state.
S.refuse = function(entry: any, event: any, text: string?)
    S.finish_request(entry)
    local player_id = entry.actor or 0
    if text ~= nil then
        S.message(player_id, text)
        return
    end
    if event.failure == "runtime_unavailable" then
        S.message(player_id, "Core settlement runtime is not installed in this build; nothing changed.")
    elseif event.failure == "stale_revision" then
        S.message(player_id, "Stale revision: reload with /settlement info and retry.")
    elseif event.failure == "insufficient_items" then
        S.message(player_id, "Not enough materials in your inventory; nothing was reserved.")
    elseif event.failure == "forbidden" then
        S.message(player_id, "Core refused the request (forbidden).")
    elseif event.failure == "capacity" then
        S.message(player_id, "Core capacity limit reached.")
    elseif event.failure ~= nil then
        S.message(player_id, "Core refused the request: " .. tostring(event.failure) .. ".")
    else
        S.message(player_id, "Core did not confirm the request; nothing changed.")
    end
end

S.issue_survey = function(entry: any, id: string, purpose: string, bounds: any)
    local record = settlements[id]
    record.op = entry.operation_id
    record.op_kind = "survey"
    entry.kind = "survey"
    entry.id = id
    entry.purpose = purpose
    entry.bounds = bounds
    entry.detail = string.format(
        "%d,%d,%d", bounds.min.x, bounds.min.y, bounds.min.z
    )
    solaris.survey_site(entry.request_id, entry.dimension, bounds, purpose)
end

S.issue_prepare = function(entry: any, id: string, building: any, survey: any, site_revision: number)
    entry.kind = "prepare"
    entry.id = id
    entry.building = building.name
    solaris.prepare_structure(
        entry.request_id,
        entry.operation_id,
        building.blueprint,
        { x = building.ox, y = building.oy, z = building.oz },
        building.rotation,
        survey.token,
        site_revision
    )
end

S.issue_advance = function(entry: any, id: string, building: any, plan: any)
    local stage_index = building.stage_index
    local bounds = S.stage_bounds(plan, stage_index)
    local built_in_stage = building.built - bounds.prior
    local remaining = bounds.units - built_in_stage
    local work_units = remaining
    if work_units > 512 then work_units = 512 end
    entry.kind = "advance"
    entry.id = id
    entry.building = building.name
    entry.stage = plan.order[stage_index + 1]
    solaris.advance_structure(
        entry.request_id,
        entry.operation_id,
        building.structure_id,
        entry.stage,
        building.reservation,
        building.structure_revision,
        work_units
    )
end

S.issue_status = function(entry: any, id: string, building_name: string, structure_id: string, purpose: string)
    entry.kind = "status"
    entry.id = id
    entry.building = building_name
    entry.purpose = purpose
    solaris.structure_status(entry.request_id, structure_id)
end

-- ---------------------------------------------------------------------------
-- Load
-- ---------------------------------------------------------------------------

local load_queue: { any } = {}

-- `load` is also the request kind for value reads that continue a command
-- ("squad-member", "job-plan", "building-for-build", "recover-resident", ...).
-- Only these purposes decode an index/snapshot; every other purpose must fall
-- through to `handle_value_read`, or the command stalls with no reply at all.
local LOAD_INDEX_PURPOSES = {
    settlement = true, bidx = true, ridx = true, sidx = true,
    ops = true, survey = true, site = true,
}

S.pump_loads = function(): boolean
    while S.requests_pending() < MAX_REQUESTS and #load_queue > 0 do
        local task = table.remove(load_queue, 1)
        S.read_key(task.key, "load", task)
    end
    if S.requests_pending() == 0 and #load_queue == 0 and not boot_done then
        boot_done = true
        for index = 1, #loaded_ids do
            S.schedule_cycle(loaded_ids[index])
        end
        S.start_recovery()
        return true
    end
    return false
end

S.load_settlement = function(id: string)
    if settlements[id] ~= nil then return end
    missing[id] = false
    loaded_ids[#loaded_ids + 1] = id
    load_queue[#load_queue + 1] = { id = id, key = S.settlement_key(id), purpose = "settlement" }
    load_queue[#load_queue + 1] = { id = id, key = S.building_index_key(id), purpose = "bidx" }
    load_queue[#load_queue + 1] = { id = id, key = S.resident_index_key(id), purpose = "ridx" }
    load_queue[#load_queue + 1] = { id = id, key = S.squad_index_key(id), purpose = "sidx" }
    load_queue[#load_queue + 1] = { id = id, key = S.operations_key(id), purpose = "ops" }
    load_queue[#load_queue + 1] = { id = id, key = S.survey_key(id), purpose = "survey" }
    load_queue[#load_queue + 1] = { id = id, key = S.site_key(id), purpose = "site" }
    S.pump_loads()
end

S.on_boot_index = function(value: any, version: any)
    local ids = S.decode_settlements_index(value)
    if ids == nil and value ~= nil then
        boot_done = true
        return
    end
    settlements_index = ids or {}
    S.set_version(INDEX_KEY, version)
    for index = 1, #settlements_index do
        S.load_settlement(settlements_index[index])
    end
    S.pump_loads()
end

-- ---------------------------------------------------------------------------
-- Recovery: resolve every durable intent with operation_status
-- ---------------------------------------------------------------------------

local recovery_queue: { any } = {}

S.start_recovery = function()
    for index = 1, #loaded_ids do
        local id = loaded_ids[index]
        local index_list = operation_ids[id] or {}
        for position = 1, #index_list do
            recovery_queue[#recovery_queue + 1] = { id = id, entry = index_list[position] }
        end
    end
    S.recover_next()
end

S.recover_next = function()
    while #recovery_queue > 0 and S.requests_pending() < MAX_REQUESTS do
        local task = table.remove(recovery_queue, 1)
        local id = task.id
        local intent = task.entry
        local current = operation_ids[id]
        if settlements[id] ~= nil and current ~= nil and S.pending_entry(current, intent.target) ~= nil then
            local entry = S.begin_request("recover", {
                id = id,
                target = intent.target,
                operation_id = intent.operation_id,
                operation_kind = intent.kind,
                actor = intent.actor,
                detail = intent.detail,
                recovery = true,
            })
            solaris.operation_status(entry.request_id, intent.operation_id)
        end
    end
end

S.recovery_failed = function(id: string, target: string, operation_kind: string, detail: string)
    S.clear_pending(id, target)
    local record = settlements[id]
    if record ~= nil then
        record.op = record.op == operation_kind and DONE or record.op
        record.op_kind = record.op == DONE and DONE or record.op_kind
        if record.op == DONE then record.op_kind = DONE end
    end
end

-- ---------------------------------------------------------------------------
-- Settlement-level operations
-- ---------------------------------------------------------------------------

S.start_survey = function(entry: any, id: string, purpose: string, bounds: any, dimension: string)
    local record = settlements[id]
    local op = S.operation_id(id, "survey")
    local intent = S.set_pending(id, "settlement", op, "survey", entry.actor, purpose)
    if intent == nil then
        S.message(entry.actor, "Too many pending operations; resolve them first.")
        S.finish_request(entry)
        return
    end
    S.refresh_counters(id)
    entry.id = id
    entry.operation_id = op
    entry.detail = purpose
    entry.pending = { id = id, op = op, purpose = purpose, bounds = bounds, dimension = dimension }
    S.write_settlement_bundle(id, record, {}, "survey-intent", entry)
end

S.start_adopt = function(entry: any, id: string, site_id: string)
    entry.kind = "site-query"
    entry.id = id
    entry.site_id = site_id
    solaris.query_settlement_site(entry.request_id, site_id, nil, 8)
end

S.pick_free_poi = function(id: string, kind: string, blueprint_hint: string?): any
    local site = sites[id]
    if site == nil then return nil end
    local fallback = nil
    for index = 1, #site.pois do
        local poi = site.pois[index]
        if poi.kind == kind and poi.state == "free" then
            if blueprint_hint == nil or poi.blueprint == blueprint_hint then return poi end
            if fallback == nil then fallback = poi end
        end
    end
    return fallback
end

S.start_populate = function(entry: any, id: string)
    local site = sites[id]
    if site == nil then
        S.message(entry.actor, "Adopt a deterministic site first: /settlement site / adopt.")
        S.finish_request(entry)
        return
    end
    if S.living_residents(id) >= MAX_RESIDENTS then
        S.message(entry.actor, "Resident limit reached for this settlement.")
        S.finish_request(entry)
        return
    end
    local poi = S.pick_free_poi(id, "home", nil)
    if poi == nil then
        S.message(entry.actor, "No free home point of interest; build or adopt more housing.")
        S.finish_request(entry)
        return
    end
    local op = S.operation_id(id, "reserve")
    local intent = S.set_pending(id, "resident-site", op, "reserve_poi", entry.actor, poi.poi_id)
    if intent == nil then
        S.message(entry.actor, "Too many pending operations; resolve them first.")
        S.finish_request(entry)
        return
    end
    local record = settlements[id]
    record.op = op
    record.op_kind = "reserve_poi"
    entry.pending = { id = id, op = op, poi = poi.poi_id }
    entry.kind = "write-intent"
    entry.purpose = "reserve-intent"
    entry.id = id
    entry.target = "resident-site"
    entry.detail = poi.poi_id
    entry.operation_id = op
    entry.target = "resident-site"
    S.write_settlement_bundle(id, record, {}, "reserve-intent", entry)
end

S.start_claim = function(entry: any, id: string, entity_uuid: string)
    local record = settlements[id]
    if record == nil then
        S.finish_request(entry)
        return
    end
    if S.living_residents(id) >= MAX_RESIDENTS then
        S.message(entry.actor, "Resident limit reached for this settlement.")
        S.finish_request(entry)
        return
    end
    local op = S.operation_id(id, "claim")
    local intent = S.set_pending(id, "resident-claim", op, "claim", entry.actor, DONE)
    if intent == nil then
        S.message(entry.actor, "Too many pending operations; resolve them first.")
        S.finish_request(entry)
        return
    end
    record.op = op
    record.op_kind = "claim"
    entry.kind = "write-intent"
    entry.purpose = "claim-intent"
    entry.id = id
    entry.target = "resident-claim"
    entry.detail = DONE
    entry.entity_uuid = entity_uuid
    entry.operation_id = op
    entry.target = "resident-claim"
    S.write_settlement_bundle(id, record, {}, "claim-intent", entry)
end

-- ---------------------------------------------------------------------------
-- Commands: declarations
-- ---------------------------------------------------------------------------

local COMMAND_HELP = table.concat({
    "Settlements v1:",
    "list | info [name] | create <name> <small|medium|large> | abandon <name>",
    "site [name] | adopt <name> <site_id> | survey <name> [plot|expand|restore]",
    "project <name> <blueprint> here|<x> <y> <z> [0|90|180|270]",
    "fund <name> <building> | build <name> <building> | pause|cancel <name> <building>",
    "buildings <name> | promote <name> | branch <name> <estate|fortress|town>",
    "specialize <name> [spec [spec]] | ruin|restore <name>",
    "populate <name> | claim <name> <entity_uuid> | residents <name>",
    "overview [name] | family <name> <resident> <family> | job <name> <resident> <job|none>",
    "hire <name> <resident> <militia|infantry|spearman|archer> | dismiss <name> <resident>",
    "squad <name> create|add|order|cancel|list ... | supply <name> | deposit <name>",
    "role <name> <uuid> <steward|captain|member>",
}, "\n")

S.usage = function(player_id: any, text: string)
    S.message(player_id, text)
end

-- ---------------------------------------------------------------------------
-- Command handlers
-- ---------------------------------------------------------------------------

S.command_create = function(event: any, uuid: string, words: { string })
    local name = string.lower(words[2])
    local size_class = string.lower(words[3])
    if not S.valid_name(name) then
        S.usage(event.player_id, "Name must be 2-16 chars: a-z 0-9 _ -")
        return
    end
    if SIZE_CLASS[size_class] ~= true then
        S.usage(event.player_id, "Size class must be small, medium, or large.")
        return
    end
    if settlements[name] ~= nil or missing[name] == false then
        S.usage(event.player_id, "That settlement name is taken.")
        return
    end
    if #settlements_index >= MAX_SETTLEMENTS then
        S.usage(event.player_id, "Settlement limit reached.")
        return
    end
    local record = {
        name = name,
        owner = uuid,
        size_class = size_class,
        stage = "hamlet",
        branch = "none",
        branch_level = 0,
        specs = DONE,
        condition = "inhabited",
        flags = DONE,
        pause = DONE,
        site_id = DONE,
        variant = DONE,
        site_revision = 0,
        houses = 0,
        meeting = 0,
        pop = 0,
        jobs = 0,
        food = 0,
        materials = 0,
        metal = 0,
        weapons = 0,
        money = 0,
        supply_tick = 0,
        military = 0,
        roles = { [uuid] = "owner" },
        op = DONE,
        op_kind = DONE,
        revision = 1,
        ticks = 0,
    }
    settlements[name] = record
    building_ids[name] = {}
    resident_ids[name] = {}
    squad_ids[name] = {}
    operation_ids[name] = {}
    missing[name] = false
    loaded_ids[#loaded_ids + 1] = name
    table.insert(settlements_index, name)
    table.sort(settlements_index)
    local mutations: { any } = {
        {
            key = INDEX_KEY,
            version = S.version_of(INDEX_KEY),
            value = S.encode_settlements_index(),
        },
        { key = S.settlement_key(name), version = S.version_of(S.settlement_key(name)), value = S.write_settlement(record) },
        S.optional_mutation(S.building_index_key(name), S.encode_building_index(name)),
        S.optional_mutation(S.resident_index_key(name), S.encode_resident_index(name)),
        S.optional_mutation(S.squad_index_key(name), S.encode_squad_index(name)),
        S.optional_mutation(S.operations_key(name), S.encode_operations(name)),
    }
    local ok = S.write_batch(mutations, "create", {
        kind = "create",
        actor = event.player_id,
        id = name,
        text = "Founded " .. name .. " (" .. size_class .. " hamlet).",
    }, "create-" .. S.sanitize_id(name))
    if not ok then
        settlements[name] = nil
        building_ids[name] = nil
        resident_ids[name] = nil
        squad_ids[name] = nil
        operation_ids[name] = nil
        table.remove(settlements_index, #settlements_index)
        S.usage(event.player_id, "Plugin is busy; retry.")
    end
end

S.command_info = function(event: any, uuid: string, words: { string })
    local id = words[2]
    if id == nil then
        for index = 1, #loaded_ids do
            local candidate = settlements[loaded_ids[index]]
            if candidate ~= nil and candidate.roles[uuid] ~= nil then
                id = candidate.name
                break
            end
        end
    end
    local record = id ~= nil and settlements[id] or nil
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.is_member(record, uuid) then
        S.usage(event.player_id, "You are not a member of " .. record.name .. ".")
        return
    end
    local site = sites[record.name]
    local site_text = record.site_id == DONE and "no site" or (record.site_id .. " " .. record.variant)
    local pause = record.pause == DONE and "running" or record.pause
    S.message(event.player_id, string.format(
        "%s | %s %s tier=%s pop=%d houses=%d jobs=%d food=%d money=%d specs=%s",
        record.name, record.size_class, site_text, S.available_tier(record), record.pop,
        record.houses, record.jobs, record.food, record.money, record.specs
    ))
    local gate, label = S.next_gate(record)
    if gate == nil then
        S.message(event.player_id, "No further growth gate; pause=" .. pause .. ".")
        return
    end
    local needed = S.missing_gate(record, gate)
    if #needed == 0 then
        S.message(event.player_id, "Ready for " .. label .. "; /settlement promote. pause=" .. pause)
    else
        S.message(event.player_id, "Next " .. label .. " missing: " .. table.concat(needed, ", ") .. "; pause=" .. pause)
    end
end

S.command_list = function(event: any, uuid: string)
    local rows: { string } = {}
    for index = 1, #loaded_ids do
        local record = settlements[loaded_ids[index]]
        if record ~= nil and S.is_member(record, uuid) then
            rows[#rows + 1] = record.name .. "(" .. record.stage .. ")"
        end
    end
    if #rows == 0 then
        S.usage(event.player_id, "No settlements. /settlement create <name> <small|medium|large>")
        return
    end
    S.usage(event.player_id, "Settlements: " .. table.concat(rows, ", "))
end

S.command_role = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if record.roles[uuid] ~= "owner" then
        S.usage(event.player_id, "Only the owner assigns roles.")
        return
    end
    local target = S.normalize_uuid(words[3])
    local role = string.lower(words[4])
    if target == nil then
        S.usage(event.player_id, "Use a player UUID (with or without dashes).")
        return
    end
    if target == record.owner then
        S.usage(event.player_id, "Ownership transfer is not supported.")
        return
    end
    if role == "member" then
        record.roles[target] = nil
    elseif role == "steward" or role == "captain" then
        if S.count_keys(record.roles) >= MAX_ROLES and record.roles[target] == nil then
            S.usage(event.player_id, "Role limit reached.")
            return
        end
        record.roles[target] = role
    else
        S.usage(event.player_id, "Role must be steward, captain, or member.")
        return
    end
    S.bump(record)
    if not S.write_settlement_bundle(record.name, record, {}, "roles", {
        kind = "write-simple",
        actor = event.player_id,
        text = "Roles updated for " .. record.name .. ".",
    }) then
        S.usage(event.player_id, "Plugin is busy; retry.")
    end
end

S.command_specs = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_build(record, uuid) then
        S.usage(event.player_id, "Only the owner or a steward changes specializations.")
        return
    end
    local specs: { string } = {}
    for index = 3, #words do
        local spec = string.lower(words[index])
        if SPECS[spec] ~= true or S.contains(specs, spec) then
            S.usage(event.player_id, "Up to two of: farming, forestry, ranching, fishing, mining.")
            return
        end
        specs[#specs + 1] = spec
    end
    if #specs > 2 then
        S.usage(event.player_id, "At most two specializations.")
        return
    end
    if #specs > 0 then
        local survey = surveys[record.name]
        if survey == nil then
            S.usage(event.player_id, "Survey the site first; suitability is a terrain fact.")
            return
        end
        for index = 1, #specs do
            local spec = specs[index]
            if S.committed_blueprint(record.name, SPEC_WORKPLACE[spec]) < 1 then
                S.usage(event.player_id, spec .. " needs a committed " .. SPEC_WORKPLACE[spec] .. ".")
                return
            end
            local tags = survey.resource_tags .. "+" .. survey.biome_tags
            local suitable = false
            for _, tag in ipairs(SPEC_TAGS[spec]) do
                if string.find("+" .. tags .. "+", "+" .. tag .. "+", 1, true) ~= nil then suitable = true end
            end
            if not suitable then
                S.usage(event.player_id, "Survey tags do not support " .. spec .. " here.")
                return
            end
        end
    end
    record.specs = #specs == 0 and DONE or table.concat(specs, "+")
    S.bump(record)
    if not S.write_settlement_bundle(record.name, record, {}, "specs", {
        kind = "write-simple",
        actor = event.player_id,
        text = record.name .. " specializations: " .. record.specs .. ".",
    }) then
        S.usage(event.player_id, "Plugin is busy; retry.")
    end
end

S.command_condition = function(event: any, uuid: string, words: { string }, ruined: boolean)
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_build(record, uuid) then
        S.usage(event.player_id, "Only the owner or a steward marks condition.")
        return
    end
    record.condition = ruined and "partially_ruined" or "inhabited"
    S.bump(record)
    local note = record.name .. " is now " .. record.condition .. "."
    if not ruined then
        note = note .. " Rebuilding ruined structures needs core restoration stages."
    end
    if not S.write_settlement_bundle(record.name, record, {}, "condition", {
        kind = "write-simple", actor = event.player_id, text = note,
    }) then
        S.usage(event.player_id, "Plugin is busy; retry.")
    end
end

S.command_branch = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_build(record, uuid) then
        S.usage(event.player_id, "Only the owner or a steward chooses a branch.")
        return
    end
    if record.stage ~= "developed" then
        S.usage(event.player_id, "Reach the developed stage before choosing a branch.")
        return
    end
    local branch_choice = string.lower(words[3])
    if branch_choice ~= "estate" and branch_choice ~= "fortress" and branch_choice ~= "town" then
        S.usage(event.player_id, "Branch must be estate, fortress, or town.")
        return
    end
    if record.branch == branch_choice then
        S.usage(event.player_id, "Already on the " .. branch_choice .. " branch.")
        return
    end
    local replan = record.branch ~= "none" or record.flags == "replan"
    record.branch = branch_choice
    record.branch_level = 0
    record.flags = replan and "replan" or DONE
    S.bump(record)
    local note = record.name .. " plans the " .. branch_choice .. " branch (level 0)."
    if replan then
        note = note .. " Branch change re-plans: survey and build the new district."
    end
    if not S.write_settlement_bundle(record.name, record, {}, "branch", {
        kind = "write-simple", actor = event.player_id, text = note,
    }) then
        S.usage(event.player_id, "Plugin is busy; retry.")
    end
end

S.command_promote = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_build(record, uuid) then
        S.usage(event.player_id, "Only the owner or a steward advances the level.")
        return
    end
    if record.condition ~= "inhabited" then
        S.usage(event.player_id, "A ruined settlement cannot advance; restore it first.")
        return
    end
    S.refresh_counters(record.name)
    local gate, label = S.next_gate(record)
    if gate == nil then
        S.usage(event.player_id, "Nothing left to promote.")
        return
    end
    local needed = S.missing_gate(record, gate)
    if #needed > 0 then
        S.usage(event.player_id, "Not ready for " .. tostring(label) .. ": " .. table.concat(needed, ", "))
        return
    end
    if record.stage == "hamlet" then
        record.stage = "village"
    elseif record.stage == "village" then
        record.stage = "developed"
    else
        record.branch_level = record.branch_level + 1
    end
    record.flags = DONE
    S.bump(record)
    if not S.write_settlement_bundle(record.name, record, {}, "promote", {
        kind = "write-simple",
        actor = event.player_id,
        text = record.name .. " advanced to " .. tostring(label) .. " (" .. S.available_tier(record) .. ").",
    }) then
        S.usage(event.player_id, "Plugin is busy; retry.")
    end
end

S.command_buildings = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.is_member(record, uuid) then
        S.usage(event.player_id, "You are not a member of " .. record.name .. ".")
        return
    end
    local index = building_ids[record.name] or {}
    if #index == 0 then
        S.usage(event.player_id, "No buildings yet; use site/adopt/survey/project.")
        return
    end
    for position = 1, #index do
        local entry = index[position]
        S.message(event.player_id, string.format("%s %s %s", entry.name, entry.blueprint, entry.state))
    end
end

S.command_residents = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.is_member(record, uuid) then
        S.usage(event.player_id, "You are not a member of " .. record.name .. ".")
        return
    end
    local index = resident_ids[record.name] or {}
    if #index == 0 then
        S.usage(event.player_id, "No residents yet; adopt a site and /settlement populate.")
        return
    end
    for position = 1, #index do
        local entry = index[position]
        S.message(event.player_id, string.format(
            "%s family=%s job=%s service=%s role=%s squad=%s life=%s gear=%s",
            entry.name, entry.family, entry.job, entry.service, entry.role,
            entry.squad, entry.life, entry.gear
        ))
    end
end

S.command_overview = function(event: any, uuid: string, words: { string })
    local name = words[2]
    if name == nil then
        local record, count = S.view_only_settlement(uuid)
        if record == nil then
            if count > 1 then
                S.usage(event.player_id, "You belong to several settlements; use /settlement overview <name>.")
            else
                S.usage(event.player_id, "You are not a member of any loaded settlement.")
            end
            return
        end
        name = record.name
    end
    local record = settlements[name]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.is_member(record, uuid) then
        S.usage(event.player_id, "You are not a member of " .. record.name .. ".")
        return
    end
    if not S.view_begin(event.player_id, uuid, name, 0) then
        S.usage(event.player_id, "The overview could not be opened right now; retry shortly.")
        return
    end
    S.usage(event.player_id, "Opened the " .. name .. " overview.")
end

S.command_supply = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.is_member(record, uuid) then
        S.usage(event.player_id, "You are not a member of " .. record.name .. ".")
        return
    end
    local entry = S.begin_request("inv_query", {
        id = record.name,
        actor = event.player_id,
        purpose = "supply",
    })
    solaris.query_owned_inventory(entry.request_id, { kind = "player_inventory", player_id = event.player_id }, nil)
end

S.command_deposit = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_build(record, uuid) then
        S.usage(event.player_id, "Only the owner or a steward deposits.")
        return
    end
    S.usage(event.player_id, "Deposits are not implemented: this package only reads the bound warehouse container, so nothing was deposited and no money was created.")
end

-- ---------------------------------------------------------------------------
-- Command handler: project / fund / build
-- ---------------------------------------------------------------------------

S.command_project = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_build(record, uuid) then
        S.usage(event.player_id, "Only the owner or a steward projects buildings.")
        return
    end
    local blueprint = string.lower(words[3])
    if CATALOG[blueprint] == nil then
        S.usage(event.player_id, "Unknown blueprint; see structures/*.toml in this package.")
        return
    end
    local survey = surveys[record.name]
    if survey == nil then
        S.usage(event.player_id, "Survey the plot first: /settlement survey " .. record.name .. " plot")
        return
    end
    if record.site_id == DONE then
        S.usage(event.player_id, "Adopt a deterministic site first: /settlement site / adopt.")
        return
    end
    local rotation = 0
    local x_value: number? = nil
    local y_value: number? = nil
    local z_value: number? = nil
    if words[4] == "here" or words[4] == nil then
        x_value = math.floor(event.x)
        y_value = math.floor(event.y)
        z_value = math.floor(event.z)
        rotation = tonumber(words[5] or "") or 0
    else
        x_value = tonumber(words[4])
        y_value = tonumber(words[5])
        z_value = tonumber(words[6])
        rotation = tonumber(words[7] or "") or 0
    end
    if x_value == nil or y_value == nil or z_value == nil then
        S.usage(event.player_id, "Coordinates must be integers.")
        return
    end
    local x = math.floor(x_value)
    local y = math.floor(y_value)
    local z = math.floor(z_value)
    if rotation ~= 0 and rotation ~= 90 and rotation ~= 180 and rotation ~= 270 then
        S.usage(event.player_id, "Rotation must be 0, 90, 180, or 270.")
        return
    end
    if x < survey.min_x or x > survey.max_x or z < survey.min_z or z > survey.max_z then
        S.usage(event.player_id, "Anchor is outside the surveyed plot; survey it first.")
        return
    end
    if not S.valid_name(words[2]) then
        S.usage(event.player_id, "Invalid settlement name.")
        return
    end
    local index = building_ids[record.name] or {}
    local building_name = string.lower(words[3])
    local short = string.match(blueprint, "^solaris:(.+)$") or blueprint
    building_name = string.gsub(short, "[^a-z0-9_]", "_")
    building_name = string.sub(building_name, 1, 8) .. "_" .. tostring(#index + 1)
    if #index >= MAX_BUILDINGS then
        S.usage(event.player_id, "Building limit reached for this settlement.")
        return
    end
    local plan_units = 0
    local building = {
        name = building_name,
        blueprint = blueprint,
        state = "projected",
        structure_id = DONE,
        rotation = rotation,
        ox = x, oy = y, oz = z,
        stage_index = 0,
        built = 0,
        watermark = 0,
        reservation = DONE,
        plan_hash = DONE,
        structure_revision = 0,
        op = DONE,
        op_kind = DONE,
        actor = event.player_id,
        revision = 1,
    }
    local op = S.operation_id(record.name, "prepare")
    if S.set_pending(record.name, "building:" .. building_name, op, "prepare", event.player_id, DONE) == nil then
        S.usage(event.player_id, "Too many pending operations; resolve them first.")
        return
    end
    building.op = op
    building.op_kind = "prepare"
    records[S.building_key(record.name, building_name)] = building
    S.index_add(index, { name = building_name, blueprint = blueprint, state = "projected" })
    record.op = op
    record.op_kind = "prepare"
    local mutations: { any } = {
        { key = S.building_key(record.name, building_name), version = S.version_of(S.building_key(record.name, building_name)), value = S.write_building(building) },
    }
    if not S.write_settlement_bundle(record.name, record, mutations, "prepare-intent", {
        kind = "prepare-after",
        id = record.name,
        building = building_name,
        actor = event.player_id,
        operation_id = op,
    }) then
        S.index_remove(index, building_name)
        records[S.building_key(record.name, building_name)] = nil
        S.clear_pending(record.name, "building:" .. building_name)
        record.op = DONE
        record.op_kind = DONE
        S.usage(event.player_id, "Plugin is busy; retry.")
    end
end

S.command_fund = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_build(record, uuid) then
        S.usage(event.player_id, "Only the owner or a steward funds construction.")
        return
    end
    S.read_key(S.plan_key(record.name, words[3]), "plan-for-fund", {
        id = record.name,
        building = words[3],
        actor = event.player_id,
    })
end

S.command_build = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_build(record, uuid) then
        S.usage(event.player_id, "Only the owner or a steward runs construction.")
        return
    end
    S.read_key(S.building_key(record.name, words[3]), "building-for-build", {
        id = record.name,
        building = words[3],
        actor = event.player_id,
    })
end

S.command_pause_or_cancel = function(event: any, uuid: string, words: { string }, cancel: boolean)
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_build(record, uuid) then
        S.usage(event.player_id, "Only the owner or a steward controls construction.")
        return
    end
    local building = records[S.building_key(record.name, words[3])]
    if building == nil then
        S.usage(event.player_id, "Load that building first with /settlement buildings.")
        return
    end
    local op = S.operation_id(record.name, cancel and "cancel" or "pause")
    if S.set_pending(record.name, "building:" .. building.name, op, cancel and "cancel" or "pause", event.player_id, DONE) == nil then
        S.usage(event.player_id, "Too many pending operations; resolve them first.")
        return
    end
    building.op = op
    building.op_kind = cancel and "cancel" or "pause"
    S.bump(building)
    local mutations: { any } = {
        { key = S.building_key(record.name, building.name), version = S.version_of(S.building_key(record.name, building.name)), value = S.write_building(building) },
    }
    local ok = S.write_batch(mutations, "construction-intent", {
        kind = "write-construction-intent",
        id = record.name,
        building = building.name,
        actor = event.player_id,
        operation_id = op,
        cancel = cancel,
    }, "construction")
    if not ok then
        building.op = DONE
        building.op_kind = DONE
        S.clear_pending(record.name, "building:" .. building.name)
    end
end

-- ---------------------------------------------------------------------------
-- Command handler: residents, jobs, garrison
-- ---------------------------------------------------------------------------

S.command_family = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_lead(record, uuid) then
        S.usage(event.player_id, "Only the owner or a captain manages families.")
        return
    end
    local resident = resident_ids[record.name] ~= nil and S.index_entry(resident_ids[record.name], words[3]) or nil
    if resident == nil then
        S.usage(event.player_id, "Unknown resident.")
        return
    end
    local family = string.lower(words[4])
    if not S.valid_name(family) then
        S.usage(event.player_id, "Family name must be 2-16 chars: a-z 0-9 _ -")
        return
    end
    S.read_key(S.resident_key(record.name, resident.name), "resident-family", {
        id = record.name,
        resident = resident.name,
        family = family,
        actor = event.player_id,
    })
end

S.command_job = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_build(record, uuid) then
        S.usage(event.player_id, "Only the owner or a steward assigns work.")
        return
    end
    local job = string.lower(words[4])
    if job ~= "none" and JOBS[job] ~= true then
        S.usage(event.player_id, "Job must be one of: farming, forestry, ranching, fishing, mining, construction, hauling, crafting, none.")
        return
    end
    if job ~= "none" and S.committed_blueprint(record.name, JOB_WORKPLACE[job]) < 1 then
        S.usage(event.player_id, "That job needs a committed " .. JOB_WORKPLACE[job] .. ".")
        return
    end
    S.read_key(S.resident_key(record.name, words[3]), "resident-job", {
        id = record.name,
        resident = words[3],
        job = job,
        actor = event.player_id,
    })
end

S.command_hire = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_lead(record, uuid) then
        S.usage(event.player_id, "Only the owner or a captain recruits.")
        return
    end
    local role = string.lower(words[4])
    if MILITARY_ROLES[role] ~= true then
        S.usage(event.player_id, "Role must be militia, infantry, spearman, or archer.")
        return
    end
    S.read_key(S.resident_key(record.name, words[3]), "resident-hire", {
        id = record.name,
        resident = words[3],
        role = role,
        actor = event.player_id,
    })
end

S.command_dismiss = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_lead(record, uuid) then
        S.usage(event.player_id, "Only the owner or a captain dismisses soldiers.")
        return
    end
    S.read_key(S.resident_key(record.name, words[3]), "resident-dismiss", {
        id = record.name,
        resident = words[3],
        actor = event.player_id,
    })
end

S.command_squad = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_lead(record, uuid) then
        S.usage(event.player_id, "Only the owner or a captain manages squads.")
        return
    end
    local action = string.lower(words[3] or "")
    if action == "list" then
        local index = squad_ids[record.name] or {}
        if #index == 0 then
            S.usage(event.player_id, "No squads.")
            return
        end
        for position = 1, #index do
            local entry = index[position]
            S.message(event.player_id, string.format(
                "%s %s order=%s role=%s members=%d armed=%d",
                entry.name, entry.state, entry.order, entry.role,
                entry.members or 0, entry.armed or 0
            ))
        end
        -- Listing one named squad prints each member's identity and the core
        -- handle the order path will use, so a roster can never look populated
        -- while silently carrying no handle.
        local squad_name = string.lower(words[4] or "")
        if squad_name ~= "" then
            if not S.valid_name(squad_name) or S.index_entry(index, squad_name) == nil then
                S.usage(event.player_id, "Unknown squad.")
                return
            end
            S.read_key(S.squad_key(record.name, squad_name), "squad-list", {
                id = record.name, squad = squad_name, actor = event.player_id,
            })
        end
        return
    end
    if action == "create" then
        local name = string.lower(words[4] or "")
        if not S.valid_name(name) then
            S.usage(event.player_id, "Squad name must be 2-16 chars.")
            return
        end
        local index = squad_ids[record.name] or {}
        if #index >= MAX_SQUADS then
            S.usage(event.player_id, "Squad limit reached.")
            return
        end
        if S.index_entry(index, name) ~= nil then
            S.usage(event.player_id, "That squad exists.")
            return
        end
        local squad = {
            name = name, role = DONE, formation = "line", state = "forming",
            order = DONE, roster = {}, members = {}, targets = {}, post = DONE, revision = 1,
        }
        records[S.squad_key(record.name, name)] = squad
        S.index_add(index, {
            name = name, state = "forming", order = DONE, role = DONE, members = 0, armed = 0,
        })
        local mutations: { any } = {
            { key = S.squad_key(record.name, name), version = S.version_of(S.squad_key(record.name, name)), value = S.write_squad(squad) },
        }
        if not S.write_settlement_bundle(record.name, record, mutations, "squad-create", {
            kind = "write-simple", actor = event.player_id, text = "Squad " .. name .. " formed.",
        }) then
            S.index_remove(index, name)
            records[S.squad_key(record.name, name)] = nil
        end
        return
    end
    if action == "add" then
        S.read_key(S.squad_key(record.name, words[4] or ""), "squad-add", {
            id = record.name,
            squad = words[4] or "",
            resident = words[5] or "",
            actor = event.player_id,
        })
        return
    end
    if action == "order" then
        local order = string.lower(words[5] or "")
        if ORDERS[order] ~= true then
            S.usage(event.player_id, "Order must be follow, move, hold, patrol, garrison, attack, or retreat.")
            return
        end
        S.read_key(S.squad_key(record.name, words[4] or ""), "squad-order", {
            id = record.name,
            squad = words[4] or "",
            order = order,
            x = math.floor(event.x),
            y = math.floor(event.y),
            z = math.floor(event.z),
            actor = event.player_id,
        })
        return
    end
    if action == "cancel" then
        S.read_key(S.squad_key(record.name, words[4] or ""), "squad-cancel", {
            id = record.name,
            squad = words[4] or "",
            actor = event.player_id,
        })
        return
    end
    S.usage(event.player_id, "squad <name> create|add|order|cancel|list ...")
end

S.command_claim = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_lead(record, uuid) then
        S.usage(event.player_id, "Only the owner or a captain adopts residents.")
        return
    end
    local entity_uuid = S.normalize_uuid(words[3])
    if entity_uuid == nil then
        S.usage(event.player_id, "Pass the entity UUID of a specific adult villager.")
        return
    end
    S.start_claim(S.begin_request("claim-start", { actor = event.player_id, id = record.name }), record.name, entity_uuid)
end

-- ---------------------------------------------------------------------------
-- Result application: writes
-- ---------------------------------------------------------------------------

S.can_admin = function(record: any, uuid: string): boolean
    return record.roles[uuid] ~= nil
end

S.command_sites = function(event: any, uuid: string, words: { string })
    local record = nil
    if words[2] ~= nil then
        record = settlements[words[2]]
        if record == nil then
            S.usage(event.player_id, "Settlement not found.")
            return
        end
        if not S.is_member(record, uuid) then
            S.usage(event.player_id, "You are not a member of " .. record.name .. ".")
            return
        end
    else
        for index = 1, #loaded_ids do
            local candidate = settlements[loaded_ids[index]]
            if candidate ~= nil and S.is_member(candidate, uuid) then
                record = candidate
                break
            end
        end
    end
    if record == nil then
        S.usage(event.player_id, "Name a settlement or found one first.")
        return
    end
    local entry = S.begin_request("sites", { id = record.name, actor = event.player_id })
    solaris.list_settlement_sites(entry.request_id, nil, 8)
end

S.command_adopt = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_build(record, uuid) then
        S.usage(event.player_id, "Only the owner or a steward adopts a site.")
        return
    end
    if record.op ~= DONE then
        S.usage(event.player_id, "Another settlement operation is in flight.")
        return
    end
    if not S.valid_opaque(words[3]) then
        S.usage(event.player_id, "Pass a deterministic site id from /settlement site.")
        return
    end
    S.start_adopt(S.begin_request("site-start", { id = record.name, actor = event.player_id }), record.name, words[3])
end

S.command_survey = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_build(record, uuid) then
        S.usage(event.player_id, "Only the owner or a steward surveys.")
        return
    end
    if record.op ~= DONE then
        S.usage(event.player_id, "Another settlement operation is in flight.")
        return
    end
    if record.site_id == DONE then
        S.usage(event.player_id, "Adopt a deterministic site first: /settlement site " .. record.name)
        return
    end
    local purpose = string.lower(words[3] or "plot")
    if purpose == "plot" then
        purpose = "settlement"
    elseif purpose == "expand" then
        purpose = "expansion"
    elseif purpose == "restore" then
        purpose = "restoration"
    end
    if PURPOSE[purpose] ~= true then
        S.usage(event.player_id, "Purpose must be plot, expand, or restore.")
        return
    end
    local extent = 64
    if purpose == "expansion" then extent = 128 end
    local half = math.floor(extent / 2)
    local px = math.floor(event.x)
    local py = math.floor(event.y)
    local pz = math.floor(event.z)
    local min_x = px - half
    local min_z = pz - half
    if min_x < -30000000 then min_x = -30000000 end
    if min_z < -30000000 then min_z = -30000000 end
    if min_x + extent - 1 > 30000000 then min_x = 30000000 - extent + 1 end
    if min_z + extent - 1 > 30000000 then min_z = 30000000 - extent + 1 end
    local min_y = py - 16
    if min_y < -64 then min_y = -64 end
    if min_y + 31 > 319 then min_y = 319 - 31 end
    local bounds = {
        min = { x = min_x, y = min_y, z = min_z },
        max = { x = min_x + extent - 1, y = min_y + 31, z = min_z + extent - 1 },
    }
    S.start_survey(
        S.begin_request("survey-start", { id = record.name, actor = event.player_id }),
        record.name,
        purpose,
        bounds,
        DIMENSION
    )
end

S.command_populate = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if not S.can_admin(record, uuid) then
        S.usage(event.player_id, "Only a member of " .. record.name .. " can settle residents.")
        return
    end
    S.start_populate(S.begin_request("populate-start", { id = record.name, actor = event.player_id }), record.name)
end

S.abandon_step = function(entry: any)
    local queue = entry.queue
    if #queue == 0 then
        S.message(entry.actor, "Settlement " .. entry.id .. " abandoned.")
        S.finish_request(entry)
        return
    end
    -- The last batch also drops the settlement from the workspace index, so the
    -- name becomes free again in the same atomic decision.
    local limit = MAX_DELETE_BATCH
    if #queue <= MAX_DELETE_BATCH then limit = MAX_DELETE_BATCH - 1 end
    local mutations: { any } = {}
    while #mutations < limit and #queue > 0 do
        local popped: any = table.remove(queue, 1)
        local key: string = tostring(popped)
        mutations[#mutations + 1] = { key = key, version = S.version_of(key), value = nil }
    end
    if #queue == 0 then
        local ids: { string } = {}
        for index = 1, #settlements_index do
            if settlements_index[index] ~= entry.id then ids[#ids + 1] = settlements_index[index] end
        end
        table.insert(mutations, 1, {
            key = INDEX_KEY,
            version = S.version_of(INDEX_KEY),
            value = S.encode_fields({ "v1" :: any, table.unpack(ids) }),
        })
    end
    entry.planned = mutations
    if not S.write_batch(mutations, "abandon", entry, "abandon-" .. tostring(#queue)) then
        S.finish_request(entry)
        S.message(entry.actor, "Plugin is busy; retry /settlement abandon.")
    end
end

S.command_abandon = function(event: any, uuid: string, words: { string })
    local record = settlements[words[2]]
    if record == nil then
        S.usage(event.player_id, "Settlement not found.")
        return
    end
    if record.roles[uuid] ~= "owner" then
        S.usage(event.player_id, "Only the owner can abandon a settlement.")
        return
    end
    local keys: { string } = {
        S.settlement_key(record.name),
        S.building_index_key(record.name),
        S.resident_index_key(record.name),
        S.squad_index_key(record.name),
        S.operations_key(record.name),
        S.survey_key(record.name),
        S.site_key(record.name),
    }
    local buildings = building_ids[record.name] or {}
    for index = 1, #buildings do
        keys[#keys + 1] = S.building_key(record.name, buildings[index].name)
        keys[#keys + 1] = S.plan_key(record.name, buildings[index].name)
    end
    local residents = resident_ids[record.name] or {}
    for index = 1, #residents do
        keys[#keys + 1] = S.resident_key(record.name, residents[index].name)
    end
    local squads = squad_ids[record.name] or {}
    for index = 1, #squads do
        keys[#keys + 1] = S.squad_key(record.name, squads[index].name)
    end
    local entry = S.begin_request("abandon", { id = record.name, actor = event.player_id, queue = keys })
    settlements[record.name] = nil
    building_ids[record.name] = nil
    resident_ids[record.name] = nil
    squad_ids[record.name] = nil
    operation_ids[record.name] = nil
    surveys[record.name] = nil
    sites[record.name] = nil
    local remaining: { string } = {}
    for index = 1, #loaded_ids do
        if loaded_ids[index] ~= record.name then remaining[#remaining + 1] = loaded_ids[index] end
    end
    loaded_ids = remaining
    S.abandon_step(entry)
end

S.apply_write_success = function(entry: any, event: any)
    local mutations = entry.mutations
    if mutations ~= nil then
        for index = 1, #mutations do
            local mutation = mutations[index]
            if mutation.value == nil then
                record_versions[mutation.key] = nil
            else
                record_versions[mutation.key] = event.revision
            end
        end
    end
end

-- ---------------------------------------------------------------------------
-- Public command dispatch
-- ---------------------------------------------------------------------------

S.dispatch_command = function(event: any)
    local words: { string } = {}
    for word in string.gmatch(event.arguments or "", "%S+") do words[#words + 1] = word end
    local action = string.lower(words[1] or "help")
    local uuid = S.normalize_uuid(event.uuid or "")
    if uuid == nil then return end
    if not boot_done then
        S.usage(event.player_id, "Settlements are still loading.")
        return
    end

    if action == "help" then
        S.message(event.player_id, COMMAND_HELP)
    elseif action == "list" then
        S.command_list(event, uuid)
    elseif action == "create" and #words == 3 then
        S.command_create(event, uuid, words)
    elseif action == "info" then
        S.command_info(event, uuid, words)
    elseif action == "role" and #words == 4 then
        S.command_role(event, uuid, words)
    elseif action == "specialize" and #words >= 2 and #words <= 4 then
        S.command_specs(event, uuid, words)
    elseif action == "branch" and #words == 3 then
        S.command_branch(event, uuid, words)
    elseif action == "promote" and #words == 2 then
        S.command_promote(event, uuid, words)
    elseif action == "buildings" and #words == 2 then
        S.command_buildings(event, uuid, words)
    elseif action == "residents" and #words == 2 then
        S.command_residents(event, uuid, words)
    elseif action == "overview" and #words <= 3 then
        S.command_overview(event, uuid, words)
    elseif action == "supply" and #words == 2 then
        S.command_supply(event, uuid, words)
    elseif action == "deposit" and #words == 2 then
        S.command_deposit(event, uuid, words)
    elseif action == "ruin" and #words == 2 then
        S.command_condition(event, uuid, words, true)
    elseif action == "restore" and #words == 2 then
        S.command_condition(event, uuid, words, false)
    elseif action == "site" then
        S.command_sites(event, uuid, words)
    elseif action == "adopt" and #words == 3 then
        S.command_adopt(event, uuid, words)
    elseif action == "survey" and #words >= 2 then
        S.command_survey(event, uuid, words)
    elseif action == "project" and #words >= 3 then
        S.command_project(event, uuid, words)
    elseif action == "fund" and #words == 3 then
        S.command_fund(event, uuid, words)
    elseif action == "build" and #words == 3 then
        S.command_build(event, uuid, words)
    elseif action == "pause" and #words == 3 then
        S.command_pause_or_cancel(event, uuid, words, false)
    elseif action == "cancel" and #words == 3 then
        S.command_pause_or_cancel(event, uuid, words, true)
    elseif action == "populate" and #words == 2 then
        S.command_populate(event, uuid, words)
    elseif action == "family" and #words == 4 then
        S.command_family(event, uuid, words)
    elseif action == "job" and #words == 4 then
        S.command_job(event, uuid, words)
    elseif action == "hire" and #words == 4 then
        S.command_hire(event, uuid, words)
    elseif action == "dismiss" and #words == 3 then
        S.command_dismiss(event, uuid, words)
    elseif action == "squad" and #words >= 3 then
        S.command_squad(event, uuid, words)
    elseif action == "claim" and #words == 3 then
        S.command_claim(event, uuid, words)
    elseif action == "abandon" and #words == 2 then
        S.command_abandon(event, uuid, words)
    else
        S.usage(event.player_id, "Usage: /settlement help")
    end
end

-- ---------------------------------------------------------------------------
-- Site helpers
-- ---------------------------------------------------------------------------


S.canvas_blueprint_for = function(site: any, at: any): string
    local best = "solaris:plaza_well"
    local best_distance = 1.0 / 0.0
    for index = 1, #site.buildings do
        local building = site.buildings[index]
        if CATALOG[building.blueprint_id] ~= nil then
            local distance = math.abs(building.origin[1] - at[1]) + math.abs(building.origin[3] - at[3])
            if distance < best_distance then
                best_distance = distance
                best = building.blueprint_id
            end
        end
    end
    return best
end

S.resident_name_for = function(id: string, resident: any): string
    local index = resident_ids[id] or {}
    local suffix = "r" .. tostring(#index + 1)
    local generation = resident.generation_id
    if type(generation) == "string" then
        local cleaned: string = string.gsub(string.lower(generation), "[^a-z0-9]", "")
        if #cleaned >= 6 then suffix = string.sub(cleaned, 1, 8) end
    end
    local name = suffix
    if not S.valid_name(name) then name = "resident" .. tostring(#index + 1) end
    local attempt = 0
    while S.index_entry(index, name) ~= nil and attempt < 32 do
        attempt = attempt + 1
        name = string.sub(name, 1, 10) .. tostring(attempt)
    end
    return name
end

S.ensure_building = function(id: string, name: string): any
    if name == nil then return nil end
    return records[S.building_key(id, name)]
end

-- ---------------------------------------------------------------------------
-- Core continuations
-- ---------------------------------------------------------------------------

S.issue_survey_call = function(entry: any)
    local pending = entry.pending
    entry.kind = "survey"
    entry.id = pending.id
    entry.operation_id = pending.op
    entry.purpose = pending.purpose
    entry.tick = entry.tick or 0
    solaris.survey_site(entry.request_id, pending.dimension, pending.bounds, pending.purpose)
end

S.issue_prepare_call = function(entry: any)
    local id = entry.id
    local record = settlements[id]
    local building = S.ensure_building(id, entry.building)
    local survey = surveys[id]
    if record == nil or building == nil or survey == nil then
        S.finish_request(entry)
        S.clear_pending(id, "building:" .. tostring(entry.building))
        return
    end
    entry.kind = "prepare"
    solaris.prepare_structure(
        entry.request_id,
        entry.operation_id,
        building.blueprint,
        { x = building.ox, y = building.oy, z = building.oz },
        building.rotation,
        survey.token,
        record.site_revision
    )
end

-- ---------------------------------------------------------------------------
-- Refused `project`: durable withdrawal of the projected record
--
-- `command_project` persists the building record as `projected` before the core
-- answers, because that intent bundle is the durable recovery handle. A refused
-- prepare therefore leaves a record that has no structure and can never be
-- funded, and `read_building` refuses to decode exactly that shape on reload.
-- The withdrawal rides the same intent lifecycle as the release hand-back: the
-- prepare intent is rewritten as a discard intent, one compensating batch
-- deletes the building value record and drops its index entry, and the record
-- and index entry are dropped from memory only once that batch commits. A
-- committed prepare never reaches this path, so a committed project is never
-- compensated.
-- ---------------------------------------------------------------------------

-- Persist the compensating batch: the settlement record, the building index
-- without the refused entry, the operations index still carrying the discard
-- intent, and the delete of the building value record, all atomically.
S.write_discard_batch = function(entry: any): boolean
    local id = entry.id
    local name = entry.building
    local record = settlements[id]
    if record == nil or name == nil then return false end
    record.revision = record.revision + 1
    local settlement_value = S.write_settlement(record)
    if #settlement_value > ENCODED_LIMIT then return false end
    local building_index = S.encode_building_index(id, name)
    local resident_index = S.encode_resident_index(id)
    local squad_index = S.encode_squad_index(id)
    local operations_index = S.encode_operations(id)
    if building_index == nil or resident_index == nil or squad_index == nil or operations_index == nil then
        return false
    end
    local mutations: { any } = {
        { key = S.settlement_key(id), version = S.version_of(S.settlement_key(id)), value = settlement_value },
        { key = S.building_index_key(id), version = S.version_of(S.building_index_key(id)), value = building_index },
        S.optional_mutation(S.resident_index_key(id), resident_index),
        S.optional_mutation(S.squad_index_key(id), squad_index),
        S.optional_mutation(S.operations_key(id), operations_index),
        { key = S.building_key(id, name), version = S.version_of(S.building_key(id, name)), value = nil },
    }
    if #mutations > 16 then return false end
    return S.write_batch(mutations, "discard-intent", entry, tostring(record.revision))
end

-- Put the prepare intent back exactly as the durable record still holds it.
S.restore_prepare_intent = function(entry: any): boolean
    local previous = entry.previous_intent
    if previous == nil then return false end
    local target = entry.target or ("building:" .. tostring(entry.building))
    local intent = S.operation_target_of(entry.id, target)
    if intent == nil or intent.operation_id ~= entry.operation_id then return false end
    intent.operation_id = previous.operation_id
    intent.kind = previous.kind
    intent.actor = previous.actor
    intent.detail = previous.detail
    intent.attempts = previous.attempts
    local record = settlements[entry.id]
    if record ~= nil then
        record.op = previous.operation_id
        record.op_kind = previous.kind
    end
    return true
end

-- Rewrite the refused prepare intent as a discard intent and persist the
-- compensating batch. Returns false when a newer intent owns the slot or the
-- batch could not be written; the previous handle is then restored verbatim so
-- the refusal is never lost.
S.start_discard_intent = function(entry: any): boolean
    local id = entry.id
    local target = entry.target or ("building:" .. tostring(entry.building))
    local intent = S.operation_target_of(id, target)
    if intent == nil or intent.operation_id ~= entry.operation_id then return false end
    local record = settlements[id]
    local building_name = string.match(target, "^building:(.+)$")
    if record == nil or building_name == nil then return false end
    local previous = {
        operation_id = intent.operation_id,
        kind = intent.kind,
        actor = intent.actor,
        detail = intent.detail,
        attempts = intent.attempts,
    }
    local discard_op = S.operation_id(id, "discard")
    intent.operation_id = discard_op
    intent.kind = "discard"
    intent.actor = entry.actor
    intent.detail = building_name
    intent.attempts = 0
    entry.purpose = "discard-intent"
    entry.id = id
    entry.target = target
    entry.building = building_name
    entry.operation_id = discard_op
    entry.previous_intent = previous
    record.op = discard_op
    record.op_kind = "discard"
    if not S.write_discard_batch(entry) then
        S.restore_prepare_intent(entry)
        return false
    end
    return true
end

-- Every refusal listed here is checked before the single committing branch of
-- `prepare_structure`, so it is a typed non-commit for this operation. `busy`
-- and `operation_conflict` stay ambiguous on purpose: they keep the intent and
-- are re-queried rather than compensated.
S.prepare_refusal_is_pre_effect = function(failure: string?): boolean
    return failure == "blocked" or failure == "unloaded"
        or failure == "forbidden" or failure == "invalid_request"
        or failure == "not_found" or failure == "stale_revision"
        or failure == "capacity" or failure == "runtime_unavailable"
end

-- A refused prepare is withdrawn through the discard intent; an ambiguous
-- answer keeps the pending intent and re-queries the durable operation.
S.handle_prepare_refusal = function(entry: any, event: any)
    local id = entry.id
    local target = entry.target or ("building:" .. tostring(entry.building))
    entry.target = target
    local intent = S.operation_target_of(id, target)
    if intent ~= nil and intent.operation_id == entry.operation_id then
        if S.prepare_refusal_is_pre_effect(event.failure) then
            if S.start_discard_intent(entry) then return end
            S.message(entry.actor, "Plugin is busy; the refused project stays pending; retry.")
            S.finish_request(entry)
            return
        end
        S.probe_pending(id, target, entry.actor)
    end
    S.refuse(entry, event, nil)
end

-- The compensating batch did not land, so nothing durable changed: restore the
-- prepare intent the durable record still holds and re-query its operation, so
-- the refused project is retried by identity instead of erased locally.
S.handle_discard_failure = function(entry: any, event: any)
    S.restore_prepare_intent(entry)
    S.probe_pending(entry.id, entry.target, entry.actor)
    S.refuse(entry, event, "Storage did not confirm the withdrawal; the refused project stays pending and is re-queried.")
end

-- The compensating batch committed: the projected record and its index entry
-- are gone durably, so drop them from memory and persist the cleared intent.
S.finish_discard = function(entry: any)
    local id = entry.id
    local name = entry.building
    local target = entry.target or ("building:" .. tostring(name))
    local intent = S.operation_target_of(id, target)
    if intent == nil or intent.operation_id ~= entry.operation_id then
        S.finish_request(entry)
        return
    end
    S.clear_pending(id, target, entry.operation_id)
    local record = settlements[id]
    if record ~= nil then
        record.op = DONE
        record.op_kind = DONE
    end
    local text = "Withdrawn the refused project " .. tostring(name) .. "; it is no longer projected."
    if name ~= nil then
        S.index_remove(building_ids[id] or {}, name)
        records[S.building_key(id, name)] = nil
    end
    S.finish_request(entry)
    if record == nil then
        S.message(entry.actor, text)
        return
    end
    if not S.write_settlement_bundle(id, record, {}, "simple", {
        kind = "write-simple", actor = entry.actor, id = id, text = text,
    }) then
        S.message(entry.actor, text)
    end
end

S.issue_advance_call = function(entry: any)
    local id = entry.id
    local building = S.ensure_building(id, entry.building)
    local plan = records[S.plan_key(id, entry.building)]
    if building == nil or plan == nil or building.structure_id == DONE or building.reservation == DONE then
        S.finish_request(entry)
        S.clear_pending(id, "building:" .. tostring(entry.building))
        return
    end
    local bounds = S.stage_bounds(plan, building.stage_index)
    local built_in_stage = building.built - bounds.prior
    local remaining = bounds.units - built_in_stage
    local work_units = remaining
    if work_units > 512 then work_units = 512 end
    if work_units <= 0 then
        S.finish_request(entry)
        return
    end
    entry.kind = "advance"
    entry.stage = plan.order[building.stage_index + 1]
    solaris.advance_structure(
        entry.request_id,
        entry.operation_id,
        building.structure_id,
        entry.stage,
        building.reservation,
        building.structure_revision,
        work_units
    )
end

S.issue_construction_call = function(entry: any)
    local id = entry.id
    local building = S.ensure_building(id, entry.building)
    if building == nil or building.structure_id == DONE then
        S.finish_request(entry)
        S.clear_pending(id, "building:" .. tostring(entry.building))
        return
    end
    if entry.cancel then
        entry.kind = "cancel"
        solaris.cancel_structure(entry.request_id, entry.operation_id, building.structure_id, building.structure_revision)
    else
        entry.kind = "pause"
        solaris.pause_structure(entry.request_id, entry.operation_id, building.structure_id, building.structure_revision)
    end
end

S.issue_reserve_call = function(entry: any)
    local id = entry.id
    local record = settlements[id]
    local site = sites[id]
    if record == nil or site == nil then
        S.finish_request(entry)
        S.clear_pending(id, "resident-site", entry.operation_id)
        return
    end
    entry.kind = "reserve_poi"
    solaris.reserve_resident_site(entry.request_id, entry.operation_id, record.site_id, entry.detail, site.revision)
end

S.issue_spawn_call = function(entry: any)
    entry.kind = "spawn"
    solaris.spawn_resident(entry.request_id, entry.operation_id, entry.token, { kind = "villager" })
end

S.issue_claim_call = function(entry: any)
    entry.kind = "claim"
    solaris.claim_resident(entry.request_id, entry.operation_id, entry.actor, entry.entity_uuid, 0)
end

S.issue_release_call = function(entry: any)
    entry.kind = "release_site"
    -- A release started from a recovery probe keeps entry.recovery == true, and
    -- the committed-result hydration at the top of the success path rewrites
    -- entry.kind from entry.operation_kind. Leave that stale as `spawn` and the
    -- committed release is misread as a fresh reservation: no release is
    -- finished, a second spawn intent is parked, and the durable index never
    -- clears. Keep the recovery kind in step with the call this entry makes.
    entry.operation_kind = "release_site"
    solaris.release_resident_site(entry.request_id, entry.operation_id, entry.token)
end

-- Hand one refused spawn back to the core as a durable release intent. The
-- pending entry is rewritten to carry the release operation id and persisted
-- before the call, so a restart re-queries the release by its own identity.
-- Returns false when the slot belongs to a newer intent or the bundle write
-- failed; the previous recovery handle is then restored verbatim.
S.start_release_intent = function(entry: any): boolean
    local id = entry.id
    local intent = S.operation_target_of(id, "resident-site")
    if intent == nil or intent.operation_id ~= entry.operation_id then return false end
    local record = settlements[id]
    local token = intent.detail
    if record == nil or token == nil then return false end
    local previous = {
        operation_id = intent.operation_id,
        kind = intent.kind,
        actor = intent.actor,
        detail = intent.detail,
        attempts = intent.attempts,
    }
    local release_op = S.operation_id(id, "release")
    intent.operation_id = release_op
    intent.kind = "release_site"
    intent.actor = entry.actor
    intent.detail = token
    intent.attempts = 0
    entry.purpose = "release-intent"
    entry.id = id
    entry.target = "resident-site"
    entry.operation_id = release_op
    entry.token = token
    record.op = release_op
    record.op_kind = "release_site"
    if not S.write_settlement_bundle(id, record, {}, "release-intent", entry) then
        -- Never lose the only handle: put the previous intent back so a later
        -- recovery can still resolve the refused spawn.
        intent.operation_id = previous.operation_id
        intent.kind = previous.kind
        intent.actor = previous.actor
        intent.detail = previous.detail
        intent.attempts = previous.attempts
        record.op = previous.operation_id
        record.op_kind = previous.kind
        return false
    end
    return true
end

-- Resolve a spawn receipt that did not commit. An absent or ambiguous answer
-- keeps the reservation held and re-queries the durable operation; a slot
-- owned by a newer intent is left untouched.
S.handle_spawn_refusal = function(entry: any, event: any)
    local id = entry.id
    local intent = S.operation_target_of(id, "resident-site")
    if intent ~= nil and intent.operation_id == entry.operation_id then
        if S.spawn_refusal_is_pre_effect(event.failure) then
            if S.start_release_intent(entry) then return end
            S.message(entry.actor, "Plugin is busy; the site reservation stays held; retry.")
            S.finish_request(entry)
            return
        end
        S.probe_pending(id, "resident-site", entry.actor)
    end
    S.refuse(entry, event, nil)
end

-- The release reached confirmed terminal handling: drop the durable intent and
-- hand the settlement operation slot back.
S.finish_release_site = function(entry: any, text: string)
    local id = entry.id
    local intent = S.operation_target_of(id, "resident-site")
    if intent == nil or intent.operation_id ~= entry.operation_id then
        S.finish_request(entry)
        return
    end
    S.clear_pending(id, "resident-site", entry.operation_id)
    local record = settlements[id]
    if record == nil then
        S.finish_request(entry)
        return
    end
    record.op = DONE
    record.op_kind = DONE
    S.finish_request(entry)
    if not S.write_settlement_bundle(id, record, {}, "simple", {
        kind = "write-simple", actor = entry.actor, id = id, text = text,
    }) then
        S.message(entry.actor, text)
    end
end

-- A release receipt that is not a commit is only terminal when the core
-- answered with a definite ledger outcome. Anything else keeps the release
-- intent so the hand-back is retried by its own operation id.
S.handle_release_refusal = function(entry: any, event: any)
    local id = entry.id
    local intent = S.operation_target_of(id, "resident-site")
    if intent ~= nil and intent.operation_id == entry.operation_id then
        if event.failure == "not_found" or event.failure == "forbidden" then
            S.finish_release_site(entry, "The site reservation is no longer held by this settlement.")
            return
        end
        S.probe_pending(id, "resident-site", entry.actor)
    end
    S.refuse(entry, event, nil)
end

S.issue_pois_call = function(entry: any)
    local id = entry.id
    local resident = records[S.resident_key(id, entry.resident)]
    if resident == nil or resident.handle == DONE then
        S.finish_request(entry)
        S.clear_pending(id, "resident:" .. tostring(entry.resident))
        return
    end
    entry.kind = "pois"
    solaris.set_resident_pois(
        entry.request_id,
        entry.operation_id,
        resident.handle,
        resident.home_poi == DONE and nil or resident.home_poi,
        resident.work_poi == DONE and nil or resident.work_poi,
        resident.meeting_poi == DONE and nil or resident.meeting_poi,
        resident.revision
    )
end

-- ---------------------------------------------------------------------------
-- Resident assignment state machine
-- ---------------------------------------------------------------------------

S.write_resident_bundle = function(id: string, resident: any, actor: number, text: string, mutations: { any })
    local record = settlements[id]
    if record == nil then return end
    local all: { any } = {
        { key = S.resident_key(id, resident.name), version = S.version_of(S.resident_key(id, resident.name)), value = S.write_resident(resident) },
    }
    for index = 1, #mutations do all[#all + 1] = mutations[index] end
    if not S.write_settlement_bundle(id, record, all, "simple", {
        kind = "write-simple", actor = actor, id = id, text = text,
    }) then
        S.message(actor, "Plugin is busy; retry.")
    end
end

-- ---------------------------------------------------------------------------
-- C4: physical work, gear, orders and demobilisation
--
-- The plugin names a bounded target and real inputs; core executes. Every
-- reported outcome is the committed result, never a projected one.
-- ---------------------------------------------------------------------------

S.roster_entry = function(squad: any, name: string): any
    for index = 1, #squad.roster do
        if squad.roster[index].name == name then return squad.roster[index] end
    end
    return nil
end

S.refresh_squad_index = function(id: string, squad: any)
    local entry = S.index_entry(squad_ids[id] or {}, squad.name)
    if entry == nil then return end
    local residents = resident_ids[id] or {}
    local armed = 0
    for position = 1, #squad.roster do
        local member = S.index_entry(residents, squad.roster[position].name)
        if member ~= nil and member.gear ~= nil and member.gear ~= DONE then armed = armed + 1 end
    end
    entry.state = squad.state
    entry.order = squad.order
    entry.role = squad.role
    entry.members = #squad.roster
    entry.armed = armed
end

-- Compact summary of a core inventory snapshot, capped for the derived index.
S.gear_summary = function(slots: any): string
    local parts: { string } = {}
    for index = 1, #slots do
        local slot = slots[index]
        if slot.item ~= nil then
            local path = string.match(slot.item.resource_id, ":([a-z0-9_./-]+)$")
            parts[#parts + 1] = path or slot.item.resource_id
        end
    end
    if #parts == 0 then return DONE end
    local text = table.concat(parts, ",")
    if #text > MAX_GEAR_BYTES then text = string.sub(text, 1, MAX_GEAR_BYTES - 1) .. "+" end
    return text
end

S.committed_building = function(id: string, blueprint: string): any
    local index = building_ids[id] or {}
    for position = 1, #index do
        local entry = index[position]
        if entry.blueprint == blueprint and entry.state == "committed" then return entry end
    end
    return nil
end

S.active_project = function(id: string): any
    local index = building_ids[id] or {}
    for position = 1, #index do
        local entry = index[position]
        if entry.state == "funded" or entry.state == "building" then return entry end
    end
    return nil
end

S.site_anchor = function(id: string): any
    local site = sites[id]
    if site == nil then return nil end
    return {
        x = site.min_x + math.floor(site.size_x / 2),
        y = site.min_y,
        z = site.min_z + math.floor(site.size_z / 2),
    }
end

S.guard_posts = function(id: string): { string }
    local posts: { string } = {}
    local site = sites[id]
    if site == nil then return posts end
    for index = 1, #site.pois do
        local poi = site.pois[index]
        if poi.kind == "guard" then posts[#posts + 1] = poi.poi_id end
    end
    return posts
end

-- One bounded work area (16x16 footprint) around the committed workplace.
S.work_area = function(building: any, spec: any): any
    return {
        dimension = DIMENSION,
        min = { x = building.ox - 7, y = building.oy + spec.y_min, z = building.oz - 7 },
        max = { x = building.ox + 8, y = building.oy + spec.y_max, z = building.oz + 8 },
    }
end

S.work_order_of = function(job: string, building: any, plan: any, handle: string): any?
    local spec = JOB_WORK[job]
    if spec == nil then return nil end
    if spec.kind == "harvest" then
        return { kind = "harvest", area = S.work_area(building, spec), tool = spec.tool }
    end
    if spec.kind == "cut_tree" then
        return { kind = "cut_tree", area = S.work_area(building, spec), tool = spec.tool }
    end
    if spec.kind == "mine" then
        return { kind = "mine", area = S.work_area(building, spec), tool = spec.tool }
    end
    if spec.kind == "fish" then
        return { kind = "fish", area = S.work_area(building, spec), tool = spec.tool }
    end
    if spec.kind == "tend_livestock" then
        return { kind = "tend_livestock", area = S.work_area(building, spec), feed = spec.feed }
    end
    if spec.kind == "construct" then
        if building == nil or plan == nil or building.structure_id == DONE then return nil end
        local stage = plan.order[building.stage_index + 1]
        if stage == nil then return nil end
        return {
            kind = "construct", structure_id = building.structure_id, stage = stage,
            expected_revision = building.structure_revision,
        }
    end
    if spec.kind == "haul" then
        if handle == DONE then return nil end
        -- No writable settlement store exists yet, so the only real bounded
        -- move is between the resident's own canonical endpoints.
        return {
            kind = "haul",
            source = { kind = "resident_carry", handle = handle },
            destination = { kind = "resident_equipment", handle = handle },
        }
    end
    if spec.kind == "craft" then
        return { kind = "craft", recipe = spec.recipe, count = spec.units }
    end
    return nil
end

S.refresh_resident_index = function(id: string, name: string, fields: any)
    local entry = S.index_entry(resident_ids[id] or {}, name)
    if entry == nil then return end
    for key, value in pairs(fields) do entry[key] = value end
end

-- ---------------------------------------------------------------- work

S.start_job = function(id: string, resident: any, job: string, actor: number)
    if resident.life ~= "alive_loaded" and resident.life ~= "alive_unloaded" then
        S.message(actor, "That resident is not alive; cannot assign work.")
        return
    end
    if job == "none" and resident.job == DONE then
        S.message(actor, resident.name .. " has no assigned work.")
        return
    end
    if job ~= "none" and resident.service == "military" then
        S.message(actor, "Serving soldiers have no civilian job; dismiss them first.")
        return
    end
    if resident.handle == DONE then
        S.message(actor, "That resident has no core handle; claim or populate it first.")
        return
    end
    local spec = JOB_WORK[job]
    local building = nil
    if spec ~= nil and spec.kind == "construct" then
        local index_entry = S.active_project(id)
        if index_entry == nil then
            S.message(actor, "No funded or building project to construct; fund one first.")
            return
        end
        building = records[S.building_key(id, index_entry.name)]
        if building == nil then
            S.read_key(S.building_key(id, index_entry.name), "load", {
                id = id, key = S.building_key(id, index_entry.name), purpose = "job-building",
                resident = resident.name, job = job, actor = actor,
            })
            return
        end
    elseif spec ~= nil and spec.shop ~= nil then
        local index_entry = S.committed_building(id, spec.shop)
        if index_entry == nil then
            S.message(actor, "That job needs a committed " .. spec.shop .. ".")
            return
        end
        building = records[S.building_key(id, index_entry.name)]
        if building == nil then
            S.read_key(S.building_key(id, index_entry.name), "load", {
                id = id, key = S.building_key(id, index_entry.name), purpose = "job-building",
                resident = resident.name, job = job, actor = actor,
            })
            return
        end
    end
    if spec ~= nil and spec.kind == "construct" and records[S.plan_key(id, building.name)] == nil then
        S.read_key(S.plan_key(id, building.name), "load", {
            id = id, key = S.plan_key(id, building.name), purpose = "job-plan",
            resident = resident.name, job = job, building = building.name, actor = actor,
        })
        return
    end
    local entry = S.begin_request("inv_query", {
        id = id, resident = resident.name, handle = resident.handle, job = job,
        building = building ~= nil and building.name or DONE, actor = actor, purpose = "work-revision",
    })
    solaris.query_owned_inventory(entry.request_id, { kind = "resident_equipment", handle = resident.handle }, nil)
end

S.issue_work_intent = function(entry: any, revision: number)
    local id = entry.id
    local resident = records[S.resident_key(id, entry.resident)]
    local record = settlements[id]
    if resident == nil or record == nil then
        S.finish_request(entry)
        return
    end
    local op = S.operation_id(id, "work")
    if S.set_pending(id, "resident:" .. resident.name, op, "work", entry.actor, entry.job) == nil then
        S.finish_request(entry)
        S.message(entry.actor, "Too many pending operations; resolve them first.")
        return
    end
    if not S.write_settlement_bundle(id, record, {}, "work-intent", {
        kind = "work-intent", id = id, resident = resident.name, job = entry.job,
        building = entry.building or DONE, core_revision = revision,
        actor = entry.actor, operation_id = op,
    }) then
        S.clear_pending(id, "resident:" .. resident.name)
        S.message(entry.actor, "Plugin is busy; retry.")
        S.finish_request(entry)
    end
end

S.issue_work_call = function(entry: any)
    local id = entry.id
    local resident = records[S.resident_key(id, entry.resident)]
    if resident == nil or resident.handle == DONE then
        S.finish_request(entry)
        S.clear_pending(id, "resident:" .. tostring(entry.resident))
        return
    end
    if entry.job == "none" then
        entry.kind = "work"
        solaris.cancel_resident_work(entry.request_id, entry.operation_id, resident.handle, entry.core_revision)
        return
    end
    local building = nil
    if entry.building ~= nil and entry.building ~= DONE then
        building = S.ensure_building(id, entry.building)
    end
    local plan = building ~= nil and records[S.plan_key(id, building.name)] or nil
    local work = S.work_order_of(entry.job, building, plan, resident.handle)
    if work == nil then
        S.clear_pending(id, "resident:" .. resident.name)
        S.message(entry.actor, "That job has no valid bounded target yet; nothing was assigned.")
        S.finish_request(entry)
        return
    end
    entry.kind = "work"
    local spec = JOB_WORK[entry.job]
    solaris.assign_resident_work(
        entry.request_id, entry.operation_id, resident.handle, work, spec.units, entry.core_revision
    )
end

-- ---------------------------------------------------------------- gear/hire

S.start_hire = function(id: string, resident: any, role: string, actor: number)
    if resident.life ~= "alive_loaded" and resident.life ~= "alive_unloaded" then
        S.message(actor, "That resident is not alive; cannot recruit.")
        return
    end
    if resident.handle == DONE then
        S.message(actor, "That resident has no core handle; claim or populate it first.")
        return
    end
    local kit = ROLE_KIT[role]
    if kit == nil then return end
    local entry = S.begin_request("inv_query", {
        id = id, resident = resident.name, handle = resident.handle, role = role,
        actor = actor, kit = kit, purpose = "hire-kit",
    })
    solaris.query_owned_inventory(entry.request_id, { kind = "player_inventory", player_id = actor }, nil)
end

S.issue_hire_transfer = function(entry: any)
    local transfers: { any } = {}
    for index = 1, #entry.kit do
        local want = entry.kit[index]
        local endpoint = want.endpoint == "equipment"
            and { kind = "resident_equipment", handle = entry.handle }
            or { kind = "resident_carry", handle = entry.handle }
        transfers[#transfers + 1] = {
            source = { kind = "player_inventory", player_id = entry.actor },
            source_slot = entry.source_slots[index],
            destination = endpoint,
            destination_slot = entry.dest_slots[index],
            count = want.count,
        }
    end
    local revisions: { any } = {
        { endpoint = { kind = "player_inventory", player_id = entry.actor }, fence = entry.player_fence },
        { endpoint = { kind = "resident_equipment", handle = entry.handle }, fence = entry.equipment_fence },
    }
    if entry.carry_fence ~= nil then
        revisions[#revisions + 1] = { endpoint = { kind = "resident_carry", handle = entry.handle }, fence = entry.carry_fence }
    end
    entry.kind = "hire"
    entry.purpose = "hire"
    entry.operation_id = S.operation_id(entry.id, "hire")
    solaris.transfer_owned_items(entry.request_id, entry.operation_id, entry.actor, transfers, revisions)
end

-- ---------------------------------------------------------------- dismiss

S.start_dismiss = function(id: string, resident: any, actor: number)
    if resident.life ~= "alive_loaded" and resident.life ~= "alive_unloaded" then
        S.message(actor, "That resident is not alive; cannot dismiss.")
        return
    end
    if resident.handle == DONE then
        S.message(actor, "That resident has no core handle; nothing to dismiss.")
        return
    end
    if resident.service ~= "military" and resident.service ~= "demobilizing" then
        S.message(actor, "That resident is not serving.")
        return
    end
    if resident.service == "demobilizing" then
        S.start_gear_return(id, resident, actor)
        return
    end
    local entry = S.begin_request("inv_query", {
        id = id, resident = resident.name, handle = resident.handle, actor = actor, purpose = "dismiss-revision",
    })
    solaris.query_owned_inventory(entry.request_id, { kind = "resident_equipment", handle = resident.handle }, nil)
end

S.issue_dismiss_intent = function(entry: any, revision: number)
    local id = entry.id
    local resident = records[S.resident_key(id, entry.resident)]
    local record = settlements[id]
    if resident == nil or record == nil then
        S.finish_request(entry)
        return
    end
    local op = S.operation_id(id, "dismiss")
    if S.set_pending(id, "resident:" .. resident.name, op, "dismiss", entry.actor, DONE) == nil then
        S.finish_request(entry)
        S.message(entry.actor, "Too many pending operations; resolve them first.")
        return
    end
    if not S.write_settlement_bundle(id, record, {}, "dismiss-intent", {
        kind = "dismiss-intent", id = id, resident = resident.name, core_revision = revision,
        actor = entry.actor, operation_id = op,
    }) then
        S.clear_pending(id, "resident:" .. resident.name)
        S.message(entry.actor, "Plugin is busy; retry.")
        S.finish_request(entry)
    end
end

S.issue_dismiss_call = function(entry: any)
    local id = entry.id
    local resident = records[S.resident_key(id, entry.resident)]
    if resident == nil or resident.handle == DONE then
        S.finish_request(entry)
        S.clear_pending(id, "resident:" .. tostring(entry.resident))
        return
    end
    entry.kind = "dismiss"
    solaris.demobilize_resident(entry.request_id, entry.operation_id, resident.handle, entry.core_revision)
end

-- Return committed gear through the same C1 transfers that equipped it. When no
-- reachable store can take the items the resident stays demobilising with its
-- gear intact, never losing a stack.
S.start_gear_return = function(id: string, resident: any, actor: number)
    local entry = S.begin_request("inv_query", {
        id = id, resident = resident.name, handle = resident.handle, actor = actor, purpose = "return-equipment",
    })
    solaris.query_owned_inventory(entry.request_id, { kind = "resident_equipment", handle = resident.handle }, nil)
end

S.finish_gear_return = function(entry: any, civilian: boolean, text: string)
    local id = entry.id
    local resident = records[S.resident_key(id, tostring(entry.resident))]
    if resident == nil then
        S.finish_request(entry)
        return
    end
    local extra: { any } = {}
    if civilian then
        resident.service = "civilian"
        resident.role = DONE
        resident.squad = DONE
        S.refresh_resident_index(id, resident.name, {
            service = resident.service, role = DONE, squad = DONE, gear = DONE,
        })
        local squads = squad_ids[id] or {}
        for position = 1, #squads do
            local squad = records[S.squad_key(id, squads[position].name)]
            if squad ~= nil and S.roster_entry(squad, resident.name) ~= nil then
                local remaining: { any } = {}
                for member_index = 1, #squad.roster do
                    if squad.roster[member_index].name ~= resident.name then
                        remaining[#remaining + 1] = squad.roster[member_index]
                    end
                end
                squad.roster = remaining
                squad.members = {}
                for member_index = 1, #remaining do squad.members[member_index] = remaining[member_index].name end
                S.refresh_squad_index(id, squad)
                extra[#extra + 1] = {
                    key = S.squad_key(id, squad.name),
                    version = S.version_of(S.squad_key(id, squad.name)),
                    value = S.write_squad(squad),
                }
            end
        end
    else
        resident.service = "demobilizing"
        S.refresh_resident_index(id, resident.name, { service = resident.service })
    end
    S.write_resident_bundle(id, resident, entry.actor or 0, text, extra)
    S.finish_request(entry)
end

S.plan_gear_return = function(entry: any, inventory: any)
    local occupied: { any } = {}
    for index = 1, #entry.equipment_slots do
        local slot = entry.equipment_slots[index]
        if slot.item ~= nil then
            occupied[#occupied + 1] = {
                endpoint = { kind = "resident_equipment", handle = entry.handle },
                slot = slot.slot, item = slot.item,
            }
        end
    end
    for index = 1, #entry.carry_slots do
        local slot = entry.carry_slots[index]
        if slot.item ~= nil then
            occupied[#occupied + 1] = {
                endpoint = { kind = "resident_carry", handle = entry.handle },
                slot = slot.slot, item = slot.item,
            }
        end
    end
    if #occupied == 0 then
        S.finish_gear_return(entry, true, entry.resident .. " dismissed; the resident held no gear.")
        return
    end
    local free: { number } = {}
    for index = 1, #inventory.slots do
        if inventory.slots[index].item == nil then free[#free + 1] = inventory.slots[index].slot end
    end
    if #free < #occupied then
        S.finish_gear_return(entry, false, entry.resident
            .. " stays demobilising: no reachable storage can take the gear, so every item remains on the resident.")
        return
    end
    local transfers: { any } = {}
    local count = 0
    for index = 1, #occupied do
        transfers[#transfers + 1] = {
            source = occupied[index].endpoint,
            source_slot = occupied[index].slot,
            destination = { kind = "player_inventory", player_id = entry.actor },
            destination_slot = free[index],
            count = occupied[index].item.count,
        }
        count = count + occupied[index].item.count
    end
    entry.kind = "return"
    entry.purpose = "return"
    entry.returned_count = count
    entry.operation_id = S.operation_id(entry.id, "return")
    solaris.transfer_owned_items(entry.request_id, entry.operation_id, entry.actor, transfers, {
        { endpoint = { kind = "player_inventory", player_id = entry.actor }, fence = inventory.fence },
        { endpoint = { kind = "resident_equipment", handle = entry.handle }, fence = entry.equipment_fence },
        { endpoint = { kind = "resident_carry", handle = entry.handle }, fence = entry.carry_fence },
    })
end

S.handle_transfer_result = function(entry: any, _result: any)
    if entry.purpose == "hire" then
        local id = entry.id
        local resident = records[S.resident_key(id, entry.resident)]
        if resident == nil then
            S.finish_request(entry)
            return
        end
        -- The committed gear is read back from core for the recorded summary.
        entry.purpose = "hire-summary"
        solaris.query_owned_inventory(entry.request_id, { kind = "resident_equipment", handle = entry.handle }, nil)
        return
    end
    if entry.purpose == "return" then
        S.finish_gear_return(entry, true, tostring(entry.resident) .. " dismissed; " .. tostring(entry.returned_count)
            .. " real item(s) returned to your inventory. Resident handle and housing kept.")
        return
    end
    S.finish_request(entry)
end

-- ---------------------------------------------------------------- orders

S.squad_allies = function(id: string): { string }
    local record = settlements[id]
    local allies: { string } = {}
    local seen: any = {}
    local function add(value: any)
        if value == nil or value == DONE or type(value) ~= "string" then return end
        if #value == 0 or #value > 64 or seen[value] ~= nil then return end
        seen[value] = true
        allies[#allies + 1] = value
    end
    if record ~= nil then
        add(record.owner)
        for uuid in pairs(record.roles) do add(uuid) end
    end
    local squads = squad_ids[id] or {}
    for position = 1, #squads do
        local squad = records[S.squad_key(id, squads[position].name)]
        if squad ~= nil then
            for index = 1, #squad.roster do add(squad.roster[index].handle) end
        end
    end
    table.sort(allies)
    return allies
end

S.patrol_waypoints = function(id: string, y: number): { any }
    local site = sites[id]
    local waypoints: { any } = {}
    if site == nil or site.size_x < 3 or site.size_z < 3 then return waypoints end
    local x0 = site.min_x + 1
    local x1 = site.min_x + site.size_x - 1
    local z0 = site.min_z + 1
    local z1 = site.min_z + site.size_z - 1
    waypoints[1] = { x = x0, y = y, z = z0 }
    waypoints[2] = { x = x1, y = y, z = z0 }
    waypoints[3] = { x = x1, y = y, z = z1 }
    waypoints[4] = { x = x0, y = y, z = z1 }
    return waypoints
end

S.build_order = function(id: string, squad: any, order: string, actor: number, at: any): any?
    local formation = { kind = squad.formation, spacing = SQUAD_FORMATION_SPACING }
    if order == "follow" then
        return { kind = "follow", target_player = actor, formation = formation }
    end
    -- The ordering player's own position is a walkable on-surface anchor; the
    -- site anchor is only a fallback.
    local anchor = at or S.site_anchor(id)
    if order == "move" or order == "hold" or order == "retreat" then
        if anchor == nil then return nil end
        if order == "move" then
            return { kind = "move", dimension = DIMENSION, anchor = anchor, heading_degrees = 0, formation = formation }
        end
        if order == "hold" then
            return {
                kind = "hold", anchor = anchor, heading_degrees = 0, formation = formation,
                engagement_radius = SQUAD_ENGAGEMENT_RADIUS,
            }
        end
        return { kind = "retreat", anchor = anchor, formation = formation }
    end
    if order == "patrol" then
        local y = anchor ~= nil and anchor.y or 64
        local waypoints = S.patrol_waypoints(id, y)
        if #waypoints < 2 then return nil end
        return {
            kind = "patrol", waypoints = waypoints, formation = formation,
            engagement_radius = SQUAD_ENGAGEMENT_RADIUS,
        }
    end
    if order == "garrison" then
        local posts = S.guard_posts(id)
        if #posts == 0 then return nil end
        return { kind = "garrison", posts = posts, engagement_radius = SQUAD_ENGAGEMENT_RADIUS }
    end
    if order == "attack" then
        local targets: { any } = {}
        for index = 1, #squad.targets do
            targets[index] = { target_ref = squad.targets[index], policy_revision = 0, expires_revision = 0 }
        end
        if #targets == 0 then return nil end
        return {
            kind = "attack", targets = targets,
            policy = { revision = SQUAD_POLICY_REVISION, allies = S.squad_allies(id), permitted = { "hostile" } },
        }
    end
    return nil
end

S.issue_order_call = function(entry: any)
    local id = entry.id
    local squad = records[S.squad_key(id, entry.squad or "")]
    if squad == nil then
        S.clear_pending(id, "squad:" .. tostring(entry.squad))
        S.finish_request(entry)
        return
    end
    local handles: { string } = {}
    local revisions: { number } = {}
    for index = 1, #squad.roster do
        local member = squad.roster[index]
        if member.handle ~= DONE then
            handles[#handles + 1] = member.handle
            revisions[#revisions + 1] = member.order_revision
        end
    end
    if #handles == 0 then
        S.clear_pending(id, "squad:" .. squad.name)
        S.message(entry.actor, "Squad " .. squad.name .. " has no member with a core handle.")
        S.finish_request(entry)
        return
    end
    if entry.cancel then
        entry.kind = "cancel-order"
        solaris.cancel_resident_order(entry.request_id, entry.operation_id, handles, revisions)
    else
        entry.kind = "order"
        solaris.issue_resident_order(entry.request_id, entry.operation_id, handles, revisions, entry.order_payload)
    end
end

S.name_for_handle = function(squad: any, handle: string): string
    for index = 1, #squad.roster do
        if squad.roster[index].handle == handle then return squad.roster[index].name end
    end
    return string.sub(handle, 1, 8)
end

S.apply_work_result = function(entry: any, assignment: any)
    local id = entry.id
    local name = entry.resident or string.match(tostring(entry.target or ""), "^resident:(.+)$")
    if name == nil then
        S.finish_request(entry)
        return
    end
    local resident = records[S.resident_key(id, name)]
    if resident == nil then
        S.finish_request(entry)
        return
    end
    local job = entry.job or entry.detail or DONE
    if job == "none" then job = DONE end
    resident.job = job
    S.refresh_resident_index(id, name, { job = resident.job })
    S.clear_pending(id, "resident:" .. name)
    local text = name .. " work " .. resident.job .. " " .. tostring(assignment.state)
    if assignment.reason ~= nil then text = text .. " (" .. tostring(assignment.reason) .. ")" end
    text = text .. string.format(" %d/%d units", assignment.work_units_done, assignment.work_units_planned)
    if #assignment.changes > 0 then
        local parts: { string } = {}
        for index = 1, #assignment.changes do
            parts[index] = string.format("%s %+d", assignment.changes[index].item_id, assignment.changes[index].delta)
        end
        text = text .. "; committed: " .. table.concat(parts, ", ")
    else
        text = text .. "; no item committed"
    end
    S.write_resident_bundle(id, resident, entry.actor or 0, text, {})
    S.finish_request(entry)
end

S.apply_work_cancelled = function(entry: any)
    local id = entry.id
    local name = entry.resident or string.match(tostring(entry.target or ""), "^resident:(.+)$")
    if name == nil then
        S.finish_request(entry)
        return
    end
    local resident = records[S.resident_key(id, name)]
    if resident == nil then
        S.finish_request(entry)
        return
    end
    resident.job = DONE
    S.refresh_resident_index(id, name, { job = DONE })
    S.clear_pending(id, "resident:" .. name)
    S.write_resident_bundle(id, resident, entry.actor or 0, name .. " work cancelled; no further units will be committed.", {})
    S.finish_request(entry)
end

S.apply_demobilized = function(entry: any, resident_result: any)
    local id = entry.id
    local name = entry.resident or string.match(tostring(entry.target or ""), "^resident:(.+)$")
    if name == nil then
        S.finish_request(entry)
        return
    end
    local resident = records[S.resident_key(id, name)]
    if resident == nil then
        S.finish_request(entry)
        return
    end
    if resident_result.state == "civilian" then
        S.finish_gear_return(entry, true, name .. " returned to civilian life as the same resident; housing kept.")
        return
    end
    resident.service = "demobilizing"
    S.refresh_resident_index(id, name, { service = "demobilizing" })
    local reason = resident_result.reason ~= nil and tostring(resident_result.reason) or "no_storage"
    local text = name .. " is demobilising (" .. reason .. "); returning the committed gear through a real transfer."
    S.message(entry.actor or 0, text)
    S.start_gear_return(id, resident, entry.actor or 0)
    S.finish_request(entry)
end

S.apply_order_result = function(entry: any, result: any)
    local id = entry.id
    local name = entry.squad or string.match(tostring(entry.target or ""), "^squad:(.+)$")
    local squad = name ~= nil and records[S.squad_key(id, name)] or nil
    if squad == nil then
        S.finish_request(entry)
        return
    end
    local cancelled = result.kind == "order_cancelled"
    if cancelled then
        for index = 1, #squad.roster do squad.roster[index].order_revision = 0 end
        squad.targets = {}
        squad.order = DONE
        squad.state = "forming"
    else
        for index = 1, #squad.roster do squad.roster[index].order_revision = result.order_revision end
        local targets: { string } = {}
        local seen: any = {}
        for index = 1, #result.members do
            local member = result.members[index]
            for target_index = 1, #member.targets do
                local ref = member.targets[target_index].target_ref
                if seen[ref] == nil and #targets < MAX_SQUAD_TARGETS then
                    seen[ref] = true
                    targets[#targets + 1] = ref
                end
            end
        end
        squad.targets = targets
    end
    S.refresh_squad_index(id, squad)
    S.clear_pending(id, "squad:" .. squad.name)
    local lines: { string } = {}
    for index = 1, #result.members do
        local member = result.members[index]
        local line = S.name_for_handle(squad, member.handle) .. "=" .. tostring(member.state)
        if member.formation_slot ~= nil then line = line .. "#" .. tostring(member.formation_slot) end
        if #member.targets > 0 then line = line .. " targets=" .. tostring(#member.targets) end
        lines[#lines + 1] = line
    end
    local text = "Squad " .. squad.name
    if cancelled then
        text = text .. " cancelled, revision " .. tostring(result.order_revision)
    else
        text = text .. " order " .. tostring(squad.order) .. ", revision " .. tostring(result.order_revision)
    end
    if #lines > 0 then text = text .. ": " .. table.concat(lines, " ") end
    if not cancelled then text = text .. "; stored targets=" .. tostring(#squad.targets) end
    local record = settlements[id]
    local actor = entry.actor or 0
    local stored = record ~= nil and S.write_settlement_bundle(id, record, {
        { key = S.squad_key(id, squad.name), version = S.version_of(S.squad_key(id, squad.name)), value = S.write_squad(squad) },
    }, "simple", { kind = "write-simple", actor = actor, id = id, text = text })
    if not stored then
        S.message(actor, "Squad order committed; the plugin is busy and will reconcile the record on reload.")
    end
    if not cancelled then
        for index = 1, #result.combat do
            local event = result.combat[index]
            S.message(actor, string.format(
                "combat: %s hit %s for %d milli%s",
                S.name_for_handle(squad, event.attacker_handle), event.victim_target_ref,
                event.damage_milli, event.killed and " (killed)" or ""
            ))
        end
    end
    S.finish_request(entry)
end

S.handle_resident_order_result = function(entry: any, result: any)
    if result.kind == "work" then
        S.apply_work_result(entry, result.assignment)
        return
    end
    if result.kind == "work_cancelled" then
        S.apply_work_cancelled(entry)
        return
    end
    if result.kind == "demobilized" then
        S.apply_demobilized(entry, result.resident)
        return
    end
    if result.kind == "order" or result.kind == "order_cancelled" then
        S.apply_order_result(entry, result)
        return
    end
    S.finish_request(entry)
end

S.handle_core_refusal = function(entry: any, event: any)
    local id = entry.id
    local target = entry.target or ("resident:" .. tostring(entry.resident))
    if entry.kind == "order" or entry.kind == "cancel-order" then
        local squad = entry.squad ~= nil and records[S.squad_key(id, entry.squad)] or nil
        if squad ~= nil then
            local name = entry.squad
            S.clear_pending(id, "squad:" .. name)
            squad.order = entry.previous_order or DONE
            squad.state = entry.previous_state or "forming"
            S.refresh_squad_index(id, squad)
            local record = settlements[id]
            if record ~= nil then
                S.write_settlement_bundle(id, record, {
                    { key = S.squad_key(id, name), version = S.version_of(S.squad_key(id, name)), value = S.write_squad(squad) },
                }, "simple", {
                    kind = "write-simple", actor = entry.actor or 0, id = id,
                    text = "Core refused the squad order; the squad record is unchanged.",
                })
            end
        else
            S.clear_pending(id, "squad:" .. tostring(entry.squad))
        end
    else
        S.clear_pending(id, target)
    end
    local payload = event.payload
    if payload ~= nil and payload.kind == "resident_order" and payload.result ~= nil
        and (payload.result.kind == "order" or payload.result.kind == "order_cancelled") then
        local lines: { string } = {}
        for index = 1, #payload.result.members do
            local member = payload.result.members[index]
            lines[#lines + 1] = string.sub(member.handle, 1, 8) .. "=" .. tostring(member.state)
        end
        S.refuse(entry, event, "Core refused the order: " .. table.concat(lines, " "))
        return
    end
    S.refuse(entry, event, nil)
end

S.start_advance = function(id: string, building: any, actor: number)
    if building.state ~= "funded" and building.state ~= "building" then
        S.message(actor, "Fund the project first: /settlement fund " .. id .. " " .. building.name)
        return
    end
    if building.structure_id == DONE then
        S.message(actor, "That project has no core structure; re-project it.")
        return
    end
    local op = S.operation_id(id, "advance")
    if S.set_pending(id, "building:" .. building.name, op, "advance", actor, DONE) == nil then
        S.message(actor, "Too many pending operations; resolve them first.")
        return
    end
    building.op = op
    building.op_kind = "advance"
    building.actor = actor
    local record = settlements[id]
    if record == nil then return end
    local mutations: { any } = {
        { key = S.building_key(id, building.name), version = S.version_of(S.building_key(id, building.name)), value = S.write_building(building) },
    }
    if not S.write_settlement_bundle(id, record, mutations, "advance-intent", {
        kind = "advance-intent", id = id, building = building.name, actor = actor, operation_id = op,
    }) then
        S.clear_pending(id, "building:" .. building.name)
        building.op = DONE
        building.op_kind = DONE
        S.message(actor, "Plugin is busy; retry.")
    end
end

-- ---------------------------------------------------------------------------
-- Value-record reads that continue a command
-- ---------------------------------------------------------------------------

S.heal_index = function(id: string)
    local ids: { string } = {}
    for index = 1, #settlements_index do
        if settlements_index[index] ~= id then ids[#ids + 1] = settlements_index[index] end
    end
    settlements_index = ids
    S.write_batch({
        { key = INDEX_KEY, version = S.version_of(INDEX_KEY), value = S.encode_settlements_index() },
    }, "heal-index", { kind = "write", actor = 0 }, "heal-" .. S.sanitize_id(id))
end

S.handle_value_read = function(entry: any, value: any)
    -- `read_key` names the read either through `kind` or through `purpose`.
    local purpose = entry.purpose or entry.kind
    local id = entry.id
    if purpose == "overview-building" then
        local session: any = entry.session
        if session == nil then return end
        local building = S.read_building(value)
        if building == nil then
            session.stock = nil
            session.reading = false
            session.warehouse = "the stored warehouse record could not be decoded"
            S.view_present(session)
            return
        end
        records[S.building_key(id, building.name)] = building
        S.view_bind(session, building)
        return
    end
    if purpose == "plan-for-fund" then
        local plan = S.read_plan(value)
        if plan == nil then
            S.message(entry.actor, "No stored plan for that building.")
            return
        end
        records[S.plan_key(id, entry.building)] = plan
        local query = S.begin_request("inv_query", {
            id = id, building = entry.building, actor = entry.actor, purpose = "fund",
        })
        solaris.query_owned_inventory(query.request_id, { kind = "player_inventory", player_id = entry.actor }, nil)
        return
    end
    if purpose == "building-for-build" then
        local building = S.read_building(value)
        if building == nil then
            S.message(entry.actor, "No construction record for that building name.")
            return
        end
        records[S.building_key(id, building.name)] = building
        local plan = records[S.plan_key(id, building.name)]
        if plan == nil then
            S.read_key(S.plan_key(id, building.name), "load", {
                id = id, key = S.plan_key(id, building.name), purpose = "plan-for-build", building = building.name,
                actor = entry.actor,
            })
            return
        end
        S.start_advance(id, building, entry.actor)
        return
    end
    if purpose == "plan-for-build" then
        local plan = S.read_plan(value)
        local building = records[S.building_key(id, entry.building)]
        if plan == nil or building == nil then
            S.message(entry.actor, "No stored plan for that building.")
            return
        end
        records[S.plan_key(id, entry.building)] = plan
        S.start_advance(id, building, entry.actor)
        return
    end
    if purpose == "resident-job" then
        local resident = S.read_resident(value)
        if resident == nil then
            S.message(entry.actor, "Unknown resident.")
            return
        end
        records[S.resident_key(id, resident.name)] = resident
        S.start_job(id, resident, entry.job, entry.actor)
        return
    end
    if purpose == "job-building" then
        local building = S.read_building(value)
        if building == nil then
            S.message(entry.actor, "No construction record for that workplace.")
            return
        end
        records[S.building_key(id, building.name)] = building
        local resident = records[S.resident_key(id, entry.resident)]
        if resident == nil then return end
        S.start_job(id, resident, entry.job, entry.actor)
        return
    end
    if purpose == "job-plan" then
        local plan = S.read_plan(value)
        if plan == nil then
            S.message(entry.actor, "No stored plan for that building.")
            return
        end
        records[S.plan_key(id, entry.building)] = plan
        local resident = records[S.resident_key(id, entry.resident)]
        if resident == nil then return end
        S.start_job(id, resident, entry.job, entry.actor)
        return
    end
    if purpose == "resident-hire" then
        local resident = S.read_resident(value)
        if resident == nil then
            S.message(entry.actor, "Unknown resident.")
            return
        end
        records[S.resident_key(id, resident.name)] = resident
        S.start_hire(id, resident, entry.role, entry.actor)
        return
    end
    if purpose == "resident-dismiss" then
        local resident = S.read_resident(value)
        if resident == nil then
            S.message(entry.actor, "Unknown resident.")
            return
        end
        records[S.resident_key(id, resident.name)] = resident
        S.start_dismiss(id, resident, entry.actor)
        return
    end
    if purpose == "recover-resident" then
        local resident = S.read_resident(value)
        if resident ~= nil then records[S.resident_key(id, resident.name)] = resident end
        local result = entry.result
        if result == nil then
            S.finish_request(entry)
            return
        end
        if result.kind == "work" then
            S.apply_work_result(entry, result.assignment)
        elseif result.kind == "work_cancelled" then
            S.apply_work_cancelled(entry)
        elseif result.kind == "demobilized" then
            S.apply_demobilized(entry, result.resident)
        else
            S.finish_request(entry)
        end
        return
    end
    if purpose == "recover-squad" then
        local squad = S.read_squad(value)
        if squad ~= nil then records[S.squad_key(id, squad.name)] = squad end
        if entry.result ~= nil then
            S.apply_order_result(entry, entry.result)
        else
            S.finish_request(entry)
        end
        return
    end
    if purpose == "squad-member" then
        local resident = S.read_resident(value)
        local squad = records[S.squad_key(id, entry.squad)]
        if resident == nil or squad == nil then
            S.message(entry.actor, "Unknown squad or resident.")
            return
        end
        records[S.resident_key(id, resident.name)] = resident
        if resident.service ~= "military" or resident.handle == DONE then
            S.message(entry.actor, "Only serving soldiers with a core handle join a squad.")
            return
        end
        if S.roster_entry(squad, resident.name) ~= nil then
            S.message(entry.actor, resident.name .. " is already in squad " .. squad.name .. ".")
            return
        end
        if #squad.roster >= MAX_SQUAD_MEMBERS then
            S.message(entry.actor, "Squad is full.")
            return
        end
        squad.roster[#squad.roster + 1] = {
            name = resident.name, handle = resident.handle, order_revision = 0,
        }
        squad.members[#squad.members + 1] = resident.name
        squad.state = "forming"
        S.refresh_resident_index(id, resident.name, { squad = squad.name, role = resident.role })
        S.refresh_squad_index(id, squad)
        local record = settlements[id]
        if record == nil then return end
        S.write_settlement_bundle(id, record, {
            { key = S.squad_key(id, squad.name), version = S.version_of(S.squad_key(id, squad.name)), value = S.write_squad(squad) },
        }, "simple", {
            kind = "write-simple", actor = entry.actor, id = id,
            text = resident.name .. " joined squad " .. squad.name .. ".",
        })
        return
    end
    if purpose == "resident-family" then
        local resident = S.read_resident(value)
        if resident == nil then
            S.message(entry.actor, "Unknown resident.")
            return
        end
        records[S.resident_key(id, resident.name)] = resident
        resident.family = entry.family
        local index = resident_ids[id] or {}
        local index_value = S.index_entry(index, resident.name)
        if index_value ~= nil then index_value.family = resident.family end
        S.write_resident_bundle(id, resident, entry.actor, resident.name .. " family set to " .. resident.family .. ".", {})
        return
    end
    if purpose == "cycle-building" then
        local building = S.read_building(value)
        if building == nil or building.structure_id == DONE then return end
        records[S.building_key(id, building.name)] = building
        local status = S.begin_request("status", {
            id = id, building = building.name, actor = 0, purpose = "cycle",
        })
        solaris.structure_status(status.request_id, building.structure_id)
        return
    end
    if purpose == "squad-add" then
        local squad = S.read_squad(value)
        if squad == nil then
            S.message(entry.actor, "Unknown squad.")
            return
        end
        records[S.squad_key(id, squad.name)] = squad
        -- The roster stores the real core handle, so the member record must be
        -- read before it can be added.
        S.read_key(S.resident_key(id, entry.resident), "load", {
            id = id, key = S.resident_key(id, entry.resident), purpose = "squad-member",
            squad = squad.name, resident = entry.resident, actor = entry.actor,
        })
        return
    end
    if purpose == "squad-list" then
        local squad = S.read_squad(value)
        if squad == nil then
            S.message(entry.actor, "Unknown squad.")
            return
        end
        records[S.squad_key(id, squad.name)] = squad
        if #squad.roster == 0 then
            S.message(entry.actor, "Squad " .. squad.name .. " has no members.")
            return
        end
        for index = 1, #squad.roster do
            local member = squad.roster[index]
            S.message(entry.actor, string.format("%s handle=%s order_revision=%d",
                member.name, member.handle, member.order_revision or 0))
        end
        return
    end
    if purpose == "squad-order" then
        local squad = S.read_squad(value)
        if squad == nil then
            S.message(entry.actor, "Unknown squad.")
            return
        end
        records[S.squad_key(id, squad.name)] = squad
        if #squad.roster == 0 then
            S.message(entry.actor, "Squad " .. squad.name .. " has no members.")
            return
        end
        local payload = S.build_order(id, squad, entry.order, entry.actor,
            entry.x ~= nil and { x = entry.x, y = entry.y, z = entry.z } or nil)
        if payload == nil then
            S.message(entry.actor, "Squad " .. squad.name .. " cannot execute " .. entry.order
                .. " yet (needs an adopted site, a guard post, or a perceived target from hold/patrol/garrison).")
            return
        end
        local op = S.operation_id(id, "order")
        if S.set_pending(id, "squad:" .. squad.name, op, "order", entry.actor, entry.order) == nil then
            S.message(entry.actor, "Too many pending operations; resolve them first.")
            return
        end
        entry.operation_id = op
        entry.order_payload = payload
        entry.previous_order = squad.order
        entry.previous_state = squad.state
        squad.order = entry.order
        squad.state = entry.order == "garrison" and "garrisoned" or "ordered"
        S.refresh_squad_index(id, squad)
        local record = settlements[id]
        if record == nil then return end
        if not S.write_settlement_bundle(id, record, {
            { key = S.squad_key(id, squad.name), version = S.version_of(S.squad_key(id, squad.name)), value = S.write_squad(squad) },
        }, "order-intent", {
            kind = "order-intent", id = id, squad = squad.name, actor = entry.actor, operation_id = op,
            order = entry.order, order_payload = payload,
            previous_order = entry.previous_order, previous_state = entry.previous_state,
        }) then
            S.clear_pending(id, "squad:" .. squad.name)
            S.message(entry.actor, "Plugin is busy; retry.")
        end
        return
    end
    if purpose == "squad-cancel" then
        local squad = S.read_squad(value)
        if squad == nil then
            S.message(entry.actor, "Unknown squad.")
            return
        end
        records[S.squad_key(id, squad.name)] = squad
        if #squad.roster == 0 then
            S.message(entry.actor, "Squad " .. squad.name .. " has no members.")
            return
        end
        local op = S.operation_id(id, "cancel-order")
        if S.set_pending(id, "squad:" .. squad.name, op, "cancel-order", entry.actor, DONE) == nil then
            S.message(entry.actor, "Too many pending operations; resolve them first.")
            return
        end
        entry.operation_id = op
        entry.cancel = true
        entry.previous_order = squad.order
        entry.previous_state = squad.state
        squad.order = DONE
        squad.state = "forming"
        squad.targets = {}
        S.refresh_squad_index(id, squad)
        local record = settlements[id]
        if record == nil then return end
        if not S.write_settlement_bundle(id, record, {
            { key = S.squad_key(id, squad.name), version = S.version_of(S.squad_key(id, squad.name)), value = S.write_squad(squad) },
        }, "cancel-order-intent", {
            kind = "cancel-order-intent", id = id, squad = squad.name, actor = entry.actor, operation_id = op,
            cancel = true, previous_order = entry.previous_order, previous_state = entry.previous_state,
        }) then
            S.clear_pending(id, "squad:" .. squad.name)
            S.message(entry.actor, "Plugin is busy; retry.")
        end
        return
    end
end

-- ---------------------------------------------------------------------------
-- Settlement operation results
-- ---------------------------------------------------------------------------

S.apply_receipt = function(entry: any, receipt: any): boolean
    local id = entry.id
    local building = S.ensure_building(id, entry.building)
    local plan = records[S.plan_key(id, entry.building)]
    if building == nil or plan == nil then return false end
    if receipt.sequence <= building.watermark then return false end
    local bounds = S.stage_bounds(plan, building.stage_index)
    building.built = building.built + receipt.block_count
    building.watermark = receipt.sequence
    building.structure_revision = receipt.revision
    building.state = "building"
    local built_in_stage = building.built - bounds.prior
    if built_in_stage >= bounds.units then building.stage_index = building.stage_index + 1 end
    local index = building_ids[id] or {}
    local index_value = S.index_entry(index, building.name)
    if index_value ~= nil then index_value.state = "building" end
    return true
end

S.handle_structure_snapshot = function(entry: any, structure: any)
    local id = entry.id
    local building = S.ensure_building(id, entry.building)
    local record = settlements[id]
    if building == nil or record == nil then
        S.finish_request(entry)
        return
    end
    building.structure_revision = structure.revision
    building.op = DONE
    building.op_kind = DONE
    S.clear_pending(id, "building:" .. building.name)
    if record.op == "pause" or record.op == "cancel" then
        record.op = DONE
        record.op_kind = DONE
    end
    local text
    if entry.kind == "pause" then
        building.state = "paused"
        text = building.name .. " paused at revision " .. tostring(structure.revision) .. "."
    elseif entry.kind == "cancel" then
        building.state = "cancelled"
        text = building.name .. " cancelled; built blocks stay in the world and only the remainder returns."
    elseif structure.state == "committed" then
        building.state = "committed"
        if structure.reservation_ref ~= nil then building.reservation = structure.reservation_ref end
        text = building.name .. " committed (" .. building.blueprint .. ") at revision "
            .. tostring(structure.revision) .. "."
    elseif structure.state == "paused" then
        building.state = "paused"
        text = building.name .. " paused: " .. tostring(structure.pause_reason or "site_changed") .. "."
    else
        if structure.state == "running" then building.state = "building" end
        text = building.name .. " state " .. tostring(structure.state) .. "."
    end
    local index = building_ids[id] or {}
    local index_value = S.index_entry(index, building.name)
    if index_value ~= nil then index_value.state = building.state end
    S.refresh_counters(id)
    if entry.purpose == "cycle" then
        text = nil
    end
    if not S.write_settlement_bundle(id, record, {
        { key = S.building_key(id, building.name), version = S.version_of(S.building_key(id, building.name)), value = S.write_building(building) },
    }, "simple", {
        kind = "write-simple", actor = entry.actor, id = id, text = text,
    }) then
        if entry.actor ~= 0 then S.message(entry.actor, "Plugin is busy; retry.") end
    end
    S.finish_request(entry)
end

S.handle_advance_receipt = function(entry: any, receipt: any)
    local persisted = S.apply_receipt(entry, receipt)
    local id = entry.id
    local building = S.ensure_building(id, entry.building)
    local plan = records[S.plan_key(id, entry.building)]
    local record = settlements[id]
    if not persisted or building == nil or plan == nil or record == nil then
        S.finish_request(entry)
        return
    end
    local stage_complete = building.stage_index >= #plan.order
    local next_op = S.operation_id(id, "advance")
    if stage_complete then
        -- The plan is exhausted; verify the authoritative commit state instead
        -- of trusting the local counter.
        building.op = DONE
        building.op_kind = DONE
        S.clear_pending(id, "building:" .. building.name)
        local mutations: { any } = {
            { key = S.building_key(id, building.name), version = S.version_of(S.building_key(id, building.name)), value = S.write_building(building) },
        }
        if not S.write_settlement_bundle(id, record, mutations, "verify-intent", {
            kind = "verify-intent", id = id, building = building.name, actor = entry.actor,
        }) then
            if entry.actor ~= 0 then S.message(entry.actor, "Plugin is busy; retry.") end
            S.finish_request(entry)
        end
        return
    end
    if S.set_pending(id, "building:" .. building.name, next_op, "advance", entry.actor, DONE) == nil then
        if entry.actor ~= 0 then S.message(entry.actor, "Too many pending operations; construction stopped.") end
        S.finish_request(entry)
        return
    end
    building.op = next_op
    building.op_kind = "advance"
    local mutations: { any } = {
        { key = S.building_key(id, building.name), version = S.version_of(S.building_key(id, building.name)), value = S.write_building(building) },
    }
    if not S.write_settlement_bundle(id, record, mutations, "advance-intent", {
        kind = "advance-intent", id = id, building = building.name, actor = entry.actor, operation_id = next_op,
    }) then
        S.clear_pending(id, "building:" .. building.name)
        building.op = DONE
        building.op_kind = DONE
        if entry.actor ~= 0 then S.message(entry.actor, "Plugin is busy; construction stopped.") end
        S.finish_request(entry)
    end
end

S.handle_settlement_result = function(entry: any, result: any)
    local kind = result.kind
    if kind == "warehouse" then
        -- Core answered the bind with the handle it minted for the authored
        -- container; the overview reads exactly that handle back.
        S.finish_request(entry)
        local session: any = view_opens[entry.view]
        if session == nil then return end
        local binding = result.binding
        session.structure = binding.structure_id
        session.container = binding.container_id
        S.view_query(session, binding.handle)
        return
    end
    if kind == "sites" then
        local page = result.page
        if #page.sites == 0 then
            S.message(entry.actor, "No deterministic candidates in that page; core returned an empty scan.")
        else
            for index = 1, #page.sites do
                local site = page.sites[index]
                S.message(entry.actor, string.format(
                    "%s %s origin %d,%d,%d size %d,%d,%d buildings=%d",
                    site.site_id, site.variant,
                    site.footprint_origin[1], site.footprint_origin[2], site.footprint_origin[3],
                    site.footprint_size[1], site.footprint_size[2], site.footprint_size[3],
                    #site.buildings
                ))
            end
        end
        S.finish_request(entry)
        return
    end
    if kind == "site" then
        local site = result.site
        local entries: any = {}
        local order = { "home", "work", "meeting", "guard" }
        for _, poi_kind in ipairs(order) do
            for index = 1, #site.pois do
                local poi = site.pois[index]
                if poi.kind == poi_kind and #entries < MAX_POI_ENTRIES then
                    entries[#entries + 1] = {
                        poi_id = poi.poi_id,
                        kind = poi.kind,
                        capacity = poi.capacity,
                        state = poi.state,
                        blueprint = S.canvas_blueprint_for(site, poi.at),
                    }
                end
            end
        end
        local site_record = {
            site_id = site.site_id,
            variant = site.variant,
            revision = site.revision,
            min_x = site.footprint_origin[1],
            min_y = site.footprint_origin[2],
            min_z = site.footprint_origin[3],
            size_x = site.footprint_size[1],
            size_y = site.footprint_size[2],
            size_z = site.footprint_size[3],
            pois = entries,
        }
        sites[entry.id] = site_record
        local record = settlements[entry.id]
        if record == nil then
            S.finish_request(entry)
            return
        end
        record.site_id = site.site_id
        record.variant = site.variant
        record.site_revision = site.revision
        if not S.write_settlement_bundle(entry.id, record, {
            { key = S.site_key(entry.id), version = S.version_of(S.site_key(entry.id)), value = S.write_site(site_record) },
        }, "site-write", {
            kind = "write-simple", actor = entry.actor, id = entry.id,
            text = string.format(
                "Adopted %s (%s): %d buildings, %d points of interest, revision %d.",
                site.site_id, site.variant, #site.buildings, #entries, site.revision
            ),
        }) then
            S.message(entry.actor, "Plugin is busy; retry.")
        end
        S.finish_request(entry)
        return
    end
    if kind == "resident_site" then
        if entry.kind == "release_site" then
            S.finish_release_site(entry, "The refused spawn left the site reservation free again.")
            return
        end
        local reservation = result.reservation
        local id = entry.id
        local record = settlements[id]
        local site = sites[id]
        if record == nil or site == nil then
            S.finish_request(entry)
            return
        end
        local held = S.operation_target_of(id, "resident-site")
        if held ~= nil and held.operation_id ~= entry.operation_id then
            -- A newer intent owns the slot; a stale reservation completion
            -- must not replace it.
            S.finish_request(entry)
            return
        end
        site.revision = reservation.revision
        record.site_revision = reservation.revision
        local spawn_op = S.operation_id(id, "spawn")
        if S.set_pending(id, "resident-site", spawn_op, "spawn", entry.actor, reservation.spawn_site_token) == nil then
            S.message(entry.actor, "Too many pending operations; resolve them first.")
            S.finish_request(entry)
            return
        end
        record.op = spawn_op
        record.op_kind = "spawn"
        entry.id = id
        entry.operation_id = spawn_op
        entry.token = reservation.spawn_site_token
        entry.target = "resident-site"
        if not S.write_settlement_bundle(id, record, {
            { key = S.site_key(id), version = S.version_of(S.site_key(id)), value = S.write_site(site) },
        }, "spawn-intent", entry) then
            S.clear_pending(id, "resident-site", spawn_op)
            S.message(entry.actor, "Plugin is busy; retry /settlement populate.")
            S.finish_request(entry)
        end
        return
    end
    if kind == "survey" then
        local survey = result.survey
        local id = entry.id
        local record = settlements[id]
        if record == nil then
            S.finish_request(entry)
            return
        end
        local survey_record = {
            token = survey.survey_token,
            revision = survey.revision,
            dimension = survey.dimension,
            min_x = survey.bounds.min[1], min_y = survey.bounds.min[2], min_z = survey.bounds.min[3],
            max_x = survey.bounds.max[1], max_y = survey.bounds.max[2], max_z = survey.bounds.max[3],
            usable = survey.usable_plots,
            water = survey.water_columns,
            claimed = survey.claimed,
            existing = survey.existing_structures,
            biome_tags = table.concat(survey.biome_tags, "+"),
            resource_tags = table.concat(survey.resource_tags, "+"),
            purpose = entry.purpose,
            tick = entry.tick or 0,
            plugin_revision = record.revision,
        }
        surveys[id] = survey_record
        record.op = DONE
        record.op_kind = DONE
        S.clear_pending(id, "settlement")
        if survey.chunk_availability == "unloaded" then
            record.pause = "unloaded"
        end
        if not S.write_settlement_bundle(id, record, {
            { key = S.survey_key(id), version = S.version_of(S.survey_key(id)), value = S.write_survey(survey_record) },
        }, "simple", {
            kind = "write-simple", actor = entry.actor, id = id,
            text = string.format(
                "Survey %s: plots=%d water=%d claimed=%s chunks=%s tags=[%s].",
                tostring(entry.purpose), survey.usable_plots, survey.water_columns,
                tostring(survey.claimed), tostring(survey.chunk_availability),
                table.concat(survey.resource_tags, ",")
            ),
        }) then
            S.message(entry.actor, "Plugin is busy; retry.")
        end
        S.finish_request(entry)
        return
    end
    if kind == "structure" then
        local structure = result.structure
        if entry.kind == "prepare" then
            local id = entry.id
            local building = S.ensure_building(id, entry.building)
            local record = settlements[id]
            if building == nil or record == nil then
                S.finish_request(entry)
                return
            end
            local stages: any = {}
            local order: { string } = {}
            for index = 1, #structure.stages do
                local stage = structure.stages[index]
                order[index] = stage.stage
                local materials: { string } = {}
                for position = 1, #stage.materials do
                    materials[position] = stage.materials[position].resource .. "="
                        .. tostring(stage.materials[position].quantity)
                end
                stages[stage.stage] = {
                    units = stage.work_units,
                    materials = #materials == 0 and DONE or table.concat(materials, ";"),
                }
            end
            local plan_record = {
                reservation_ref = DONE,
                plan_hash = structure.resource_plan_hash,
                order = order,
                stages = stages,
                revision = 1,
            }
            records[S.plan_key(id, building.name)] = plan_record
            building.structure_id = structure.structure_id
            building.structure_revision = structure.revision
            building.plan_hash = structure.resource_plan_hash
            building.state = "projected"
            building.op = DONE
            building.op_kind = DONE
            S.clear_pending(id, "building:" .. building.name)
            record.op = DONE
            record.op_kind = DONE
            if not S.write_settlement_bundle(id, record, {
                { key = S.building_key(id, building.name), version = S.version_of(S.building_key(id, building.name)), value = S.write_building(building) },
                { key = S.plan_key(id, building.name), version = S.version_of(S.plan_key(id, building.name)), value = S.write_plan(plan_record) },
            }, "simple", {
                kind = "write-simple", actor = entry.actor, id = id,
                text = string.format(
                    "%s projected (%s, %d stages). Fund it: /settlement fund %s %s",
                    building.name, building.blueprint, #order, id, building.name
                ),
            }) then
                S.message(entry.actor, "Plugin is busy; retry /settlement project.")
            end
            S.finish_request(entry)
            return
        end
        S.handle_structure_snapshot(entry, structure)
        return
    end
    if kind == "receipt" then
        S.handle_advance_receipt(entry, result.receipt)
        return
    end
    S.finish_request(entry)
end

S.handle_resident_result = function(entry: any, result: any)
    if result.kind == "page" then
        S.finish_request(entry)
        return
    end
    local resident = result.resident
    local id = entry.id
    if entry.kind == "spawn" or entry.kind == "claim" then
        local record = settlements[id]
        if record == nil then
            S.finish_request(entry)
            return
        end
        local name = S.resident_name_for(id, resident)
        local resident_record = {
            name = name,
            handle = resident.handle,
            generation = resident.generation_id or DONE,
            entity_uuid = resident.entity_uuid,
            family = "unassigned",
            job = DONE,
            service = "civilian",
            role = DONE,
            squad = DONE,
            house = DONE,
            home_poi = resident.pois.home or DONE,
            work_poi = resident.pois.work or DONE,
            meeting_poi = DONE,
            life = resident.lifecycle,
            op = DONE,
            op_kind = DONE,
            actor = 0,
            revision = 1,
        }
        if entry.kind == "spawn" then
            local poi = S.pick_free_poi(id, "home", nil)
            if poi ~= nil then
                poi.state = "occupied"
                resident_record.home_poi = poi.poi_id
                resident_record.house = poi.blueprint
            end
        end
        records[S.resident_key(id, name)] = resident_record
        S.index_add(resident_ids[id] or {}, {
            name = name,
            family = resident_record.family,
            job = resident_record.job,
            service = resident_record.service,
            squad = DONE,
            life = resident_record.life,
            role = DONE,
            gear = DONE,
        })
        record.op = DONE
        record.op_kind = DONE
        S.clear_pending(id, "resident-site", entry.operation_id)
        S.clear_pending(id, "resident-claim", entry.operation_id)
        S.refresh_counters(id)
        local mutations: { any } = {
            { key = S.resident_key(id, name), version = S.version_of(S.resident_key(id, name)), value = S.write_resident(resident_record) },
        }
        if sites[id] ~= nil then
            mutations[#mutations + 1] = { key = S.site_key(id), version = S.version_of(S.site_key(id)), value = S.write_site(sites[id]) }
        end
        if not S.write_settlement_bundle(id, record, mutations, "simple", {
            kind = "write-simple", actor = entry.actor, id = id,
            text = string.format(
                "%s settled in %s (%s), home %s. House capacity is tracked by that home POI.",
                name, id, tostring(resident.lifecycle), resident_record.home_poi
            ),
        }) then
            S.message(entry.actor, "Plugin is busy; the resident will be reconciled from core on reload.")
        end
        S.finish_request(entry)
        return
    end
    if entry.kind == "pois" then
        local resident_record = records[S.resident_key(id, entry.resident)]
        local record = settlements[id]
        if resident_record ~= nil and record ~= nil then
            resident_record.home_poi = resident.pois.home or DONE
            resident_record.work_poi = resident.pois.work or DONE
            resident_record.revision = resident.revision
            S.write_settlement_bundle(id, record, {
                { key = S.resident_key(id, entry.resident), version = S.version_of(S.resident_key(id, entry.resident)), value = S.write_resident(resident_record) },
            }, "simple", {
                kind = "write-simple", actor = entry.actor, id = id, text = entry.text,
            })
        end
        S.finish_request(entry)
        return
    end
    S.finish_request(entry)
end

S.handle_inventory_result = function(entry: any, result: any)
    if result.kind == "snapshot" then
        local snapshot = result.inventory
        if entry.purpose == "overview" then
            S.finish_request(entry)
            local session: any = view_opens[entry.view]
            if session == nil then return end
            local stock: any = {}
            for index = 1, #snapshot.slots do
                local slot = snapshot.slots[index]
                if slot.item ~= nil and #stock < VIEW_MAX_STOCK then
                    stock[#stock + 1] = {
                        resource_id = slot.item.resource_id,
                        count = slot.item.count,
                        slot = slot.slot,
                    }
                end
            end
            session.stock = stock
            session.reading = false
            session.warehouse = "container " .. tostring(session.container) .. " of structure "
                .. tostring(session.structure) .. ", revision " .. tostring(snapshot.fence.revision)
            S.view_present(session)
            return
        end
        if entry.purpose == "supply" then
            local id = entry.id
            local record = settlements[id]
            if record == nil then
                S.finish_request(entry)
                return
            end
            local food = 0
            local money = 0
            for index = 1, #snapshot.slots do
                local slot = snapshot.slots[index]
                if slot.item ~= nil then
                    if S.contains(FOOD_ITEMS, slot.item.resource_id) then food = food + slot.item.count end
                    if S.contains(MONEY_ITEMS, slot.item.resource_id) then money = money + slot.item.count end
                end
            end
            record.food = food
            record.money = money
            record.supply_tick = entry.tick or record.supply_tick
            if not S.write_settlement_bundle(id, record, {}, "simple", {
                kind = "write-simple", actor = entry.actor, id = id,
                text = string.format(
                    "Verified carried supply for %s: food %d, money %d (canonical inventory read, no minting).",
                    id, food, money
                ),
            }) then
                S.message(entry.actor, "Plugin is busy; retry /settlement supply.")
            end
            S.finish_request(entry)
            return
        end
        if entry.purpose == "fund" then
            local id = entry.id
            local plan = records[S.plan_key(id, entry.building)]
            local record = settlements[id]
            local building = S.ensure_building(id, entry.building)
            if plan == nil or record == nil or building == nil then
                S.finish_request(entry)
                return
            end
            local op = S.operation_id(id, "fund")
            if S.set_pending(id, "building:" .. building.name, op, "fund", entry.actor, DONE) == nil then
                S.message(entry.actor, "Too many pending operations; resolve them first.")
                S.finish_request(entry)
                return
            end
            building.op = op
            building.op_kind = "fund"
            entry.id = id
            entry.building = building.name
            entry.operation_id = op
            entry.fence = snapshot.fence
            entry.plan = plan
            if not S.write_settlement_bundle(id, record, {
                { key = S.building_key(id, building.name), version = S.version_of(S.building_key(id, building.name)), value = S.write_building(building) },
            }, "fund-intent", entry) then
                S.clear_pending(id, "building:" .. building.name)
                building.op = DONE
                building.op_kind = DONE
                S.message(entry.actor, "Plugin is busy; retry /settlement fund.")
                S.finish_request(entry)
            end
            return
        end
        if entry.purpose == "hire-kit" then
            local picks: any = {}
            local missing: { string } = {}
            for index = 1, #entry.kit do
                local want = entry.kit[index]
                local source = nil
                for position = 1, #snapshot.slots do
                    local slot = snapshot.slots[position]
                    if slot.item ~= nil and slot.item.resource_id == want.item and slot.item.count >= want.count then
                        source = slot.slot
                        break
                    end
                end
                if source == nil then
                    missing[#missing + 1] = want.item
                else
                    picks[index] = source
                end
            end
            if #missing > 0 then
                S.finish_request(entry)
                S.message(entry.actor, "Cannot equip " .. entry.resident .. " as " .. entry.role
                    .. "; missing from your inventory: " .. table.concat(missing, ", ") .. ". Nothing was equipped.")
                return
            end
            entry.source_slots = picks
            entry.player_fence = snapshot.fence
            entry.purpose = "hire-gear"
            solaris.query_owned_inventory(entry.request_id, { kind = "resident_equipment", handle = entry.handle }, nil)
            return
        end
        if entry.purpose == "hire-gear" then
            entry.equipment_fence = snapshot.fence
            local free: any = {}
            for position = 1, #snapshot.slots do
                local slot = snapshot.slots[position]
                if slot.item == nil then free[slot.slot] = true end
            end
            local dests: any = {}
            for index = 1, #entry.kit do
                local want = entry.kit[index]
                if want.endpoint == "equipment" then
                    local dest = nil
                    if free[want.slot] == true then
                        dest = want.slot
                        free[want.slot] = nil
                    else
                        for slot = 0, 5 do
                            if free[slot] == true then
                                dest = slot
                                free[slot] = nil
                                break
                            end
                        end
                    end
                    if dest == nil then
                        S.finish_request(entry)
                        S.message(entry.actor, entry.resident .. " has no free equipment slot; nothing was equipped.")
                        return
                    end
                    dests[index] = dest
                end
            end
            entry.dest_slots = dests
            local carry = false
            for index = 1, #entry.kit do
                if entry.kit[index].endpoint == "carry" then carry = true end
            end
            if carry then
                entry.purpose = "hire-carry"
                solaris.query_owned_inventory(entry.request_id, { kind = "resident_carry", handle = entry.handle }, nil)
            else
                S.issue_hire_transfer(entry)
            end
            return
        end
        if entry.purpose == "hire-carry" then
            entry.carry_fence = snapshot.fence
            local free: any = {}
            for position = 1, #snapshot.slots do
                local slot = snapshot.slots[position]
                if slot.item == nil then free[slot.slot] = true end
            end
            for index = 1, #entry.kit do
                local want = entry.kit[index]
                if want.endpoint == "carry" then
                    local dest = nil
                    if free[want.slot] == true then
                        dest = want.slot
                        free[want.slot] = nil
                    else
                        for slot = 0, 7 do
                            if free[slot] == true then
                                dest = slot
                                free[slot] = nil
                                break
                            end
                        end
                    end
                    if dest == nil then
                        S.finish_request(entry)
                        S.message(entry.actor, entry.resident .. " has no free carry slot; nothing was equipped.")
                        return
                    end
                    entry.dest_slots[index] = dest
                end
            end
            S.issue_hire_transfer(entry)
            return
        end
        if entry.purpose == "hire-summary" then
            local id = entry.id
            local resident = records[S.resident_key(id, entry.resident)]
            if resident == nil then
                S.finish_request(entry)
                return
            end
            local gear = S.gear_summary(snapshot.slots)
            resident.service = "military"
            resident.role = entry.role
            resident.job = DONE
            S.refresh_resident_index(id, resident.name, {
                service = resident.service, role = resident.role, job = DONE, gear = gear,
            })
            local squads = squad_ids[id] or {}
            for position = 1, #squads do
                local squad = records[S.squad_key(id, squads[position].name)]
                if squad ~= nil and S.roster_entry(squad, resident.name) ~= nil then
                    S.refresh_squad_index(id, squad)
                end
            end
            S.write_resident_bundle(id, resident, entry.actor, resident.name .. " serves as " .. entry.role
                .. " (core equipment: " .. gear .. ").", {})
            S.finish_request(entry)
            return
        end
        if entry.purpose == "work-revision" then
            S.issue_work_intent(entry, snapshot.fence.revision)
            return
        end
        if entry.purpose == "dismiss-revision" then
            S.issue_dismiss_intent(entry, snapshot.fence.revision)
            return
        end
        if entry.purpose == "return-equipment" then
            entry.equipment_slots = snapshot.slots
            entry.equipment_fence = snapshot.fence
            entry.purpose = "return-carry"
            solaris.query_owned_inventory(entry.request_id, { kind = "resident_carry", handle = entry.handle }, nil)
            return
        end
        if entry.purpose == "return-carry" then
            entry.carry_slots = snapshot.slots
            entry.carry_fence = snapshot.fence
            entry.purpose = "return-player"
            solaris.query_owned_inventory(entry.request_id, { kind = "player_inventory", player_id = entry.actor }, nil)
            return
        end
        if entry.purpose == "return-player" then
            S.plan_gear_return(entry, snapshot)
            return
        end
        S.finish_request(entry)
        return
    end
    if result.kind == "transfer" then
        S.handle_transfer_result(entry, result)
        return
    end
    if result.kind == "reservation" then
        local reservation = result.reservation
        local id = entry.id
        local building = S.ensure_building(id, entry.building)
        local record = settlements[id]
        if building == nil or record == nil then
            S.finish_request(entry)
            return
        end
        building.reservation = reservation.reservation_ref
        building.plan_hash = reservation.resource_plan_hash
        building.state = "funded"
        building.op = DONE
        building.op_kind = DONE
        S.clear_pending(id, "building:" .. building.name)
        local index = building_ids[id] or {}
        local index_value = S.index_entry(index, building.name)
        if index_value ~= nil then index_value.state = "funded" end
        if not S.write_settlement_bundle(id, record, {
            { key = S.building_key(id, building.name), version = S.version_of(S.building_key(id, building.name)), value = S.write_building(building) },
        }, "simple", {
            kind = "write-simple", actor = entry.actor, id = id,
            text = string.format(
                "Reserved real materials for %s (%s). Build: /settlement build %s %s",
                building.name, reservation.reservation_ref, id, building.name
            ),
        }) then
            S.message(entry.actor, "Plugin is busy; retry /settlement build.")
        end
        S.finish_request(entry)
        return
    end
    S.finish_request(entry)
end

S.handle_recover = function(entry: any, event: any)
    local id = entry.id
    local operation_kind = entry.operation_kind
    local intent = S.operation_target_of(id, entry.target)
    if entry.target == "resident-site" and intent ~= nil
        and (operation_kind == "spawn" or operation_kind == "release_site") then
        if operation_kind == "release_site"
            and (event.state == "rejected" or event.failure ~= nil)
        then
            if event.failure == "forbidden" then
                -- A definite core answer: another owner holds this token, so
                -- the hand-back can never succeed from here.
                S.finish_release_site(entry, "The site reservation is no longer held by this settlement.")
                return
            end
            -- An absent or rejected release receipt is not proof that the
            -- hand-back committed. Keep the durable intent so the reservation
            -- stays tracked and reissue the release under its own operation id:
            -- an absent receipt never becomes present by re-querying it.
            intent.attempts = (intent.attempts or 0) + 1
            if intent.attempts < MAX_RECOVERY_ATTEMPTS then
                entry.operation_id = intent.operation_id
                entry.token = intent.detail
                S.issue_release_call(entry)
            else
                S.message(entry.actor, "The site reservation hand-back is unresolved; it stays tracked.")
                S.finish_request(entry)
            end
            return
        end
        if event.state == "rejected" or event.failure ~= nil then
            if operation_kind == "spawn" and S.spawn_refusal_is_pre_effect(event.failure) then
                if S.start_release_intent(entry) then return end
            end
            -- Not a confirmed non-commit: the reservation stays held and the
            -- durable operation is re-queried through the status path.
            S.probe_pending(id, entry.target, entry.actor)
            S.finish_request(entry)
            return
        end
    end
    if event.failure == "not_found" then
        local attempts = intent ~= nil and intent.attempts or MAX_RECOVERY_ATTEMPTS
        if intent ~= nil and attempts < MAX_RECOVERY_ATTEMPTS then
            intent.attempts = attempts + 1
            local target = intent.target
            entry.operation_id = intent.operation_id
            if operation_kind == "prepare" or operation_kind == "advance" then
                local building_name = string.match(target, "^building:(.+)$")
                if building_name ~= nil then
                    entry.building = building_name
                    entry.kind = operation_kind
                    if records[S.building_key(id, building_name)] == nil then
                        S.read_key(S.building_key(id, building_name), "load", {
                            id = id, key = S.building_key(id, building_name), purpose = "building-for-build",
                            building = building_name, actor = entry.actor,
                        })
                        S.finish_request(entry)
                        return
                    end
                    if operation_kind == "prepare" then
                        S.issue_prepare_call(entry)
                    else
                        S.issue_advance_call(entry)
                    end
                    return
                end
            end
        end
        if operation_kind == "work" or operation_kind == "order" or operation_kind == "cancel-order" or operation_kind == "dismiss" then
            S.clear_pending(id, entry.target, entry.operation_id)
            S.message(entry.actor, "A previous " .. tostring(operation_kind)
                .. " intent did not commit before the restart; nothing changed. Reissue the command.")
            S.finish_request(entry)
            return
        end
        if operation_kind == "discard" then
            -- The withdrawal already committed: the compensating batch is the
            -- delete, so the intent only needs to be dropped. Unlike an
            -- interrupted project this leaves the settlement unpaused.
            S.clear_pending(id, entry.target, entry.operation_id)
            local record = settlements[id]
            if record ~= nil then
                record.op = DONE
                record.op_kind = DONE
            end
            S.finish_request(entry)
            return
        end
        S.clear_pending(id, entry.target, entry.operation_id)
        local record = settlements[id]
        if record ~= nil then
            record.op = DONE
            record.op_kind = DONE
            record.pause = "interrupted"
        end
        S.finish_request(entry)
        return
    end
    if event.state == "rejected" then
        S.clear_pending(id, entry.target, entry.operation_id)
        local record = settlements[id]
        if record ~= nil then
            record.op = DONE
            record.op_kind = DONE
        end
        S.finish_request(entry)
        return
    end
    entry.kind = operation_kind
    local building_name = string.match(entry.target, "^building:(.+)$")
    if building_name ~= nil then entry.building = building_name end
    entry.recovery = true
    local payload = event.payload
    local kind = payload ~= nil and payload.kind or nil
    if kind == "settlement" then
        S.handle_settlement_result(entry, payload.result)
    elseif kind == "resident" then
        S.handle_resident_result(entry, payload.result)
    elseif kind == "owned_inventory" then
        S.handle_inventory_result(entry, payload.result)
    elseif kind == "resident_order" then
        -- The durable receipt is the authority; load the record it belongs to
        -- so a restart still applies the committed outcome exactly once.
        local resident_name = string.match(entry.target or "", "^resident:(.+)$")
        local squad_name = string.match(entry.target or "", "^squad:(.+)$")
        if resident_name ~= nil and records[S.resident_key(id, resident_name)] == nil then
            S.read_key(S.resident_key(id, resident_name), "load", {
                id = id, key = S.resident_key(id, resident_name), purpose = "recover-resident",
                resident = resident_name, job = entry.detail, actor = entry.actor,
                operation_kind = operation_kind, result = payload.result,
            })
            return
        end
        if squad_name ~= nil and records[S.squad_key(id, squad_name)] == nil then
            S.read_key(S.squad_key(id, squad_name), "load", {
                id = id, key = S.squad_key(id, squad_name), purpose = "recover-squad",
                squad = squad_name, actor = entry.actor,
                operation_kind = operation_kind, result = payload.result,
            })
            return
        end
        S.handle_resident_order_result(entry, payload.result)
    else
        S.finish_request(entry)
    end
end

S.continue_write = function(entry: any, event: any)
    local purpose = entry.purpose
    if purpose == "create" then
        S.message(entry.actor, entry.text)
        S.schedule_cycle(entry.id)
    elseif purpose == "simple" then
        if entry.text ~= nil then S.message(entry.actor, entry.text) end
    elseif purpose == "survey-intent" then
        S.issue_survey_call(entry)
        return
    elseif purpose == "prepare-intent" then
        S.issue_prepare_call(entry)
        return
    elseif purpose == "discard-intent" then
        S.finish_discard(entry)
        return
    elseif purpose == "advance-intent" then
        S.issue_advance_call(entry)
        return
    elseif purpose == "construction-intent" then
        S.issue_construction_call(entry)
        return
    elseif purpose == "reserve-intent" then
        S.issue_reserve_call(entry)
        return
    elseif purpose == "spawn-intent" then
        S.issue_spawn_call(entry)
        return
    elseif purpose == "release-intent" then
        S.issue_release_call(entry)
        return
    elseif purpose == "claim-intent" then
        S.issue_claim_call(entry)
        return
    elseif purpose == "pois-intent" then
        S.issue_pois_call(entry)
        return
    elseif purpose == "work-intent" then
        S.issue_work_call(entry)
        return
    elseif purpose == "dismiss-intent" then
        S.issue_dismiss_call(entry)
        return
    elseif purpose == "order-intent" or purpose == "cancel-order-intent" then
        S.issue_order_call(entry)
        return
    elseif purpose == "verify-intent" then
        local building = S.ensure_building(entry.id, entry.building)
        if building ~= nil and building.structure_id ~= DONE then
            entry.kind = "status"
            entry.purpose = "verify"
            solaris.structure_status(entry.request_id, building.structure_id)
            return
        end
    elseif purpose == "fund-intent" then
        local plan = entry.plan
        local fence = entry.fence
        local building = S.ensure_building(entry.id, entry.building)
        if plan == nil or fence == nil or building == nil then
            S.message(entry.actor, "Funding intent expired; retry /settlement fund.")
        elseif plan ~= nil and fence ~= nil and building ~= nil then
            entry.kind = "fund"
            solaris.reserve_inventory_items(
                entry.request_id,
                entry.operation_id,
                { kind = "player_inventory", player_id = entry.actor },
                S.plan_to_resource_plan(plan),
                { revision = fence.revision, snapshot_hash = fence.snapshot_hash }
            )
            return
        end
    elseif purpose == "abandon" then
        S.abandon_step(entry)
        return
    elseif purpose == "heal-index" then
        return
    elseif entry.text ~= nil then
        S.message(entry.actor, entry.text)
    end
    S.finish_request(entry)
end

function on_operation_result(event: any)
    local entry: any = requests[event.request_id]
    if entry == nil then return end
    entry.tick = event.fired_tick
    if event.state == "rejected" or event.failure ~= nil then
        -- A refused warehouse read is not a chat refusal: the screen is open,
        -- so the exact reason is presented in the model instead.
        if entry.view ~= nil then
            S.finish_request(entry)
            local session: any = view_opens[entry.view]
            if session ~= nil then S.view_failed(session, event.failure or event.state) end
            return
        end
        if entry.kind == "recover" then
            S.handle_recover(entry, event)
            return
        end
        if entry.purpose == "discard-intent" then
            S.handle_discard_failure(entry, event)
            return
        end
        if entry.kind == "write" then
            S.finish_request(entry)
            if event.failure == "stale_revision" then
                S.message(entry.actor, "State changed concurrently; reload and retry.")
            elseif event.failure == "operation_conflict" then
                S.message(entry.actor, "That decision conflicts with a stored operation; nothing changed.")
            else
                S.message(entry.actor, "Storage rejected the update: " .. tostring(event.failure or "rejected") .. ".")
            end
            return
        end
        if entry.purpose == "return-equipment" or entry.purpose == "return-carry" or entry.purpose == "return-player" then
            S.finish_gear_return(entry, false, tostring(entry.resident)
                .. " stays demobilising: core refused the return, so no item left the resident.")
            return
        end
        if entry.kind == "return" then
            S.finish_gear_return(entry, false, tostring(entry.resident)
                .. " stays demobilising: core refused the return, so no item left the resident.")
            return
        end
        if entry.kind == "work" or entry.kind == "order" or entry.kind == "cancel-order" or entry.kind == "dismiss" then
            S.handle_core_refusal(entry, event)
            return
        end
        if entry.kind == "spawn" then
            S.handle_spawn_refusal(entry, event)
            return
        end
        if entry.kind == "release_site" then
            S.handle_release_refusal(entry, event)
            return
        end
        if entry.kind == "prepare" then
            S.handle_prepare_refusal(entry, event)
            return
        end
        if entry.kind == "inv_query" or entry.kind == "fund" or entry.kind == "reserve_poi" then
            if entry.kind == "reserve_poi" then S.clear_pending(entry.id, "resident-site", entry.operation_id) end
            S.finish_request(entry)
            S.refuse(entry, event, nil)
            return
        end
        S.refuse(entry, event, nil)
        return
    end
    -- A recovery probe resolves a committed intent; its durable target names
    -- the record the receipt belongs to. Hydrate the same fields the failure
    -- path sets, or a committed receipt for a building intent dereferences a
    -- nil name and the whole plugin is disabled.
    if entry.recovery == true and entry.target ~= nil then
        local target = tostring(entry.target)
        local building_name = string.match(target, "^building:(.+)$")
        if building_name ~= nil then entry.building = building_name end
        local resident_name = string.match(target, "^resident:(.+)$")
        if resident_name ~= nil then entry.resident = resident_name end
        local squad_name = string.match(target, "^squad:(.+)$")
        if squad_name ~= nil then entry.squad = squad_name end
        -- A committed settlement probe resolves as the intent it re-queried:
        -- a spawn hands the resident over, a release frees the token.
        if target == "resident-site" and entry.operation_kind ~= nil then
            entry.kind = entry.operation_kind
        end
    end
    local payload = event.payload
    local kind = payload ~= nil and payload.kind or nil
    if kind == "storage_batch" then
        S.apply_write_success(entry, event)
        S.continue_write(entry, event)
        return
    end
    if kind == "settlement" then
        S.handle_settlement_result(entry, payload.result)
        return
    end
    if kind == "resident" then
        S.handle_resident_result(entry, payload.result)
        return
    end
    if kind == "owned_inventory" then
        S.handle_inventory_result(entry, payload.result)
        return
    end
    if kind == "resident_order" then
        S.handle_resident_order_result(entry, payload.result)
        return
    end
    S.finish_request(entry)
end

function on_plugin_storage_get_result(event: any)
    local entry: any = requests[event.request_id]
    if entry == nil then return end
    S.set_version(entry.key, event.version)
    local value = event.value
    if entry.kind == "boot" then
        S.finish_request(entry)
        S.on_boot_index(value, event.version)
        return
    end
    if entry.kind == "load" and LOAD_INDEX_PURPOSES[entry.purpose] == true then
        local id = entry.id
        local purpose = entry.purpose
        local decoded_ok = true
        if purpose == "settlement" then
            local record = S.read_settlement(value)
            settlements[id] = record
            if record == nil then decoded_ok = false end
        elseif purpose == "bidx" then
            building_ids[id] = value == nil and {} or S.decode_building_index(value)
            if building_ids[id] == nil then decoded_ok = false end
        elseif purpose == "ridx" then
            resident_ids[id] = value == nil and {} or S.decode_resident_index(value)
            if resident_ids[id] == nil then decoded_ok = false end
        elseif purpose == "sidx" then
            squad_ids[id] = value == nil and {} or S.decode_squad_index(value)
            if squad_ids[id] == nil then decoded_ok = false end
        elseif purpose == "ops" then
            operation_ids[id] = value == nil and {} or S.decode_operations(value)
            if operation_ids[id] == nil then decoded_ok = false end
        elseif purpose == "survey" then
            surveys[id] = value == nil and nil or S.read_survey(value)
            if value ~= nil and surveys[id] == nil then decoded_ok = false end
        elseif purpose == "site" then
            sites[id] = value == nil and nil or S.read_site(value)
            if value ~= nil and sites[id] == nil then decoded_ok = false end
        end
        if not decoded_ok then
            settlements[id] = nil
            missing[id] = true
        elseif purpose == "settlement" and settlements[id] == nil then
            missing[id] = true
            S.heal_index(id)
        end
        S.finish_request(entry)
        S.pump_loads()
        return
    end
    S.handle_value_read(entry, value)
    S.finish_request(entry)
end

-- ---------------------------------------------------------------------------
-- Simulation-tick economy cycle
-- ---------------------------------------------------------------------------

S.schedule_cycle = function(id: string)
    if settlements[id] == nil then return end
    if cycle_timers[id] ~= nil then return end
    local timer_id = "cycle-" .. S.sanitize_id(id)
    cycle_timers[id] = timer_id
    solaris.schedule_timer(timer_id, CYCLE_TICKS)
end

S.run_cycle = function(id: string, tick: number)
    local record = settlements[id]
    if record == nil then return end
    if record.op ~= DONE then
        S.schedule_cycle(id)
        return
    end
    local verify_name = nil
    local index = building_ids[id] or {}
    for position = 1, #index do
        local state = index[position].state
        if (state == "funded" or state == "building") and verify_name == nil then
            verify_name = index[position].name
        end
    end
    if verify_name ~= nil then
        S.read_key(S.building_key(id, verify_name), "load", {
            id = id, key = S.building_key(id, verify_name), purpose = "cycle-building",
            building = verify_name, actor = 0,
        })
    end
    local entry = S.begin_request("online", { id = id, actor = 0, tick = tick })
    solaris.list_online_players(entry.request_id, 32)
    S.schedule_cycle(id)
end

function on_plugin_online_result(event: any)
    local entry: any = requests[event.request_id]
    if entry == nil then return end
    if entry.kind == "view-request" then
        S.finish_request(entry)
        S.view_open_for(entry.player_id, event.players or {})
        return
    end
    if entry.kind ~= "online" then return end
    S.finish_request(entry)
    local id = entry.id
    local record = settlements[id]
    if record == nil then return end
    local players = event.players or {}
    local tick = entry.tick or 0
    local reason = "interrupted"
    if not S.near_settlement(id, players, 32) and not S.member_online(id, players) then
        reason = "unloaded"
    elseif record.pop > 0 and record.jobs == 0 then
        reason = "no_workers"
    elseif record.supply_tick > 0 and tick - record.supply_tick > SUPPLY_TICKS then
        -- A verified carried supply is a projection, not a warehouse: when it is
        -- older than its window the counter is zeroed, never kept as free stock.
        record.food = 0
        record.materials = 0
        record.weapons = 0
        reason = "missing_input"
    elseif S.committed_role(id, "meeting") == 0 and S.committed_role(id, "work") == 0 then
        reason = "blocked_route"
    end
    record.ticks = tick
    record.pause = reason
    S.write_settlement_bundle(id, record, {}, "simple", {
        kind = "write-simple", actor = 0, id = id, text = nil,
    })
end

-- ---------------------------------------------------------------------------
-- Client view: the declared Loader settlement screen
-- ---------------------------------------------------------------------------

-- The screen the bundle declares. A `loader.view_request` for the settlement
-- kind is what the client's key opens; `/settlement overview` reuses the same
-- builder. Both send only what core can map back to a `paged_table`, a
-- `resource_panel` and the three `action_button` widgets of that screen.

S.view_clamp = function(value: string, maximum: number): string
    if #value <= maximum then return value end
    return string.sub(value, 1, maximum)
end

-- A bounded deterministic digest of an opaque id, rendered as plain decimal
-- digits. It names exactly one warehouse binding inside an operation id while
-- keeping that id inside the core's 64-byte `[a-z0-9_-]` contract.
S.view_digest = function(value: string): string
    local hash = 0
    for index = 1, #value do
        hash = (hash * 131 + string.byte(value, index)) % 2147483647
    end
    local text = ""
    repeat
        local digit = hash % 10
        text = string.sub(VIEW_DIGITS, digit + 1, digit + 1) .. text
        hash = (hash - digit) / 10
    until hash == 0
    return text
end

S.view_forget = function(session: any)
    if session.instance ~= nil then view_sessions[session.instance] = nil end
    view_opens[session.request_id] = nil
    if session.open_entry ~= nil then S.finish_request(session.open_entry) end
    for position = #view_order, 1, -1 do
        if view_order[position] == session then table.remove(view_order, position) end
    end
end

S.view_remember = function(session: any)
    while #view_order >= MAX_VIEW_SESSIONS do
        S.view_forget(table.remove(view_order, 1))
    end
    view_order[#view_order + 1] = session
end

-- The settlement a key-driven request belongs to. The request carries a player
-- id only, so the uuid comes from the online snapshot, and the screen is only
-- opened when that uuid is a member of exactly one loaded settlement.
S.view_player_uuid = function(player_id: number, players: any): string?
    for index = 1, #players do
        local player = players[index]
        if player.player_id == player_id then return S.normalize_uuid(player.uuid or "") end
    end
    return nil
end

S.view_only_settlement = function(uuid: string): (any?, number)
    local found: any = nil
    local count = 0
    for index = 1, #loaded_ids do
        local record = settlements[loaded_ids[index]]
        if record ~= nil and S.is_member(record, uuid) then
            found = record
            count = count + 1
        end
    end
    return found, count
end

-- `name` is the identity, the dimension is the one every operation of this
-- package uses, and the provenance is what adoption stored: the core site id,
-- its variant and the site revision the settlement recorded.
S.view_identity_fields = function(record: any): any
    local gate, label = S.next_gate(record)
    local needs = "no further growth gate for this branch"
    if gate ~= nil then
        local missing_requirements = S.missing_gate(record, gate)
        if #missing_requirements == 0 then
            needs = "all " .. tostring(label) .. " requirements met"
        else
            needs = "missing for " .. tostring(label) .. ": "
                .. table.concat(missing_requirements, ", ")
        end
    end
    return {
        { id = "settlement", text = record.name },
        { id = "site", text = record.site_id },
        { id = "dimension", text = DIMENSION },
        { id = "stage", text = record.stage .. " branch=" .. record.branch
            .. " level=" .. tostring(record.branch_level) },
        { id = "condition", text = record.condition },
        { id = "provenance", text = "owner=" .. record.owner .. " variant=" .. record.variant
            .. " site_revision=" .. tostring(record.site_revision) },
        { id = "cycle", text = "pause=" .. record.pause .. " tick=" .. tostring(record.ticks)
            .. " supply_tick=" .. tostring(record.supply_tick) },
        { id = "needs", text = S.view_clamp(needs, VIEW_TEXT_BYTES) },
    }
end

-- The four counters the panel declares, each the verified projection the
-- settlement record holds, against the requirement of the next growth gate.
-- A gate without that requirement shows 0, which is what it requires.
S.view_supply_entries = function(record: any): any
    local gate = S.next_gate(record)
    local function requirement(field: string): number
        if gate == nil then return 0 end
        local wanted = gate[field]
        if wanted == nil then return 0 end
        return wanted
    end
    return {
        { id = "residents", have = record.pop, need = requirement("pop") },
        { id = "food", have = record.food, need = requirement("food") },
        { id = "money", have = record.money, need = requirement("money") },
        { id = "weapons", have = record.weapons, need = requirement("weapons") },
    }
end

S.view_assignment = function(resident: any): string
    local parts: { string } = {}
    if resident.job ~= DONE then parts[#parts + 1] = resident.job end
    parts[#parts + 1] = resident.service
    if MILITARY_ROLES[resident.role] == true then parts[#parts + 1] = resident.role end
    if resident.squad ~= DONE then parts[#parts + 1] = "squad=" .. resident.squad end
    return table.concat(parts, " ")
end

-- One page of the overview: the roster slice for this cursor, or the warehouse
-- stock page, which is always the last one. `page` and `page_count` must be
-- exact integers, so the page count is integer arithmetic.
S.view_model = function(session: any): any
    local id = session.id
    local record = settlements[id]
    local index = resident_ids[id] or {}
    local roster_pages = (#index + VIEW_PAGE_ROWS - 1) // VIEW_PAGE_ROWS
    local page_count = roster_pages + 1
    local page = session.page
    if page < 0 then page = 0 end
    if page > page_count - 1 then page = page_count - 1 end
    session.page = page
    local rows: any = {}
    if page < roster_pages then
        local first = page * VIEW_PAGE_ROWS + 1
        local last = first + VIEW_PAGE_ROWS - 1
        if last > #index then last = #index end
        for position = first, last do
            local resident = index[position]
            rows[#rows + 1] = { cells = {
                S.view_clamp(resident.name, VIEW_CELL_BYTES),
                S.view_clamp(S.view_assignment(resident), VIEW_CELL_BYTES),
                S.view_clamp("life=" .. tostring(resident.life) .. " family="
                    .. tostring(resident.family) .. " gear=" .. tostring(resident.gear), VIEW_CELL_BYTES),
            } }
        end
    elseif session.stock == nil then
        -- No confirmed read yet: the row restates the exact reason instead of
        -- standing in for stock the plugin never read.
        rows[1] = { cells = {
            "warehouse",
            "0",
            S.view_clamp(session.warehouse, VIEW_CELL_BYTES),
        } }
    else
        local stock = session.stock
        if #stock == 0 then
            rows[1] = { cells = { "warehouse", "0", S.view_clamp(session.warehouse, VIEW_CELL_BYTES) } }
        end
        for position = 1, #stock do
            local stack = stock[position]
            rows[#rows + 1] = { cells = {
                S.view_clamp(stack.resource_id, VIEW_CELL_BYTES),
                tostring(stack.count),
                "slot " .. tostring(stack.slot),
            } }
        end
    end
    local fields = S.view_identity_fields(record)
    fields[#fields + 1] = { id = "warehouse", text = S.view_clamp(session.warehouse, VIEW_TEXT_BYTES) }
    local next_enabled = page + 1 < page_count
    local prev_enabled = page > 0
    local refresh_enabled = session.reading ~= true
    local actions: any = {
        { action_id = VIEW_REFRESH, enabled = refresh_enabled, label = "Refresh overview" },
        { action_id = VIEW_NEXT, enabled = next_enabled, label = "Next page" },
        { action_id = VIEW_PREV, enabled = prev_enabled, label = "Previous page" },
    }
    if not refresh_enabled then
        actions[1].deny_reason = "a warehouse read is already in flight"
    end
    if not next_enabled then
        actions[2].deny_reason = "the last page is already shown"
    end
    if not prev_enabled then
        actions[3].deny_reason = "the first page is already shown"
    end
    local model: any = {
        page = page,
        page_count = page_count,
        rows = rows,
        fields = fields,
        actions = actions,
        resource_entries = S.view_supply_entries(record),
    }
    if session.stock == nil then
        model.reason = S.view_clamp("no warehouse contents: " .. session.warehouse, VIEW_TEXT_BYTES)
    end
    return model
end

-- Present the current page. A present before core reported the instance id has
-- nowhere to go, so it is remembered and replayed once the id and the revision
-- of the open arrive.
S.view_present = function(session: any)
    if session.instance == nil then
        session.dirty = true
        return
    end
    local record = settlements[session.id]
    if record == nil then return end
    session.dirty = false
    solaris.present_client_view(
        session.player_id, session.instance, session.revision, S.view_model(session))
    -- Core accepts a present only at the revision it holds and answers with the
    -- next one; the revision a later action reports is authoritative.
    session.revision = session.revision + 1
end

S.view_failed = function(session: any, failure: string?)
    session.stock = nil
    session.reading = false
    session.warehouse = "core refused the warehouse read (" .. tostring(failure or "refused") .. ")"
    S.view_present(session)
end

-- Bind the settlement's own warehouse structure and read the bound container
-- back. The authored container ordinal is this package's choice; the handle is
-- minted by core and never guessed here.
S.view_read_warehouse = function(session: any)
    if session.reading then return end
    session.reading = true
    local id = session.id
    local index = building_ids[id] or {}
    local committed: any = nil
    local pending: any = nil
    for position = 1, #index do
        local building = index[position]
        if building.blueprint == "solaris:warehouse" then
            if building.state == "committed" then
                committed = building
                break
            end
            pending = building
        end
    end
    if committed == nil then
        session.stock = nil
        session.reading = false
        if pending ~= nil then
            session.warehouse = "warehouse " .. tostring(pending.name) .. " is "
                .. tostring(pending.state) .. ", not committed"
        else
            session.warehouse = "no solaris:warehouse building in " .. id
        end
        S.view_present(session)
        return
    end
    local building = records[S.building_key(id, committed.name)]
    if building == nil then
        if not S.read_key(S.building_key(id, committed.name), "load", {
            id = id, key = S.building_key(id, committed.name), purpose = "overview-building",
            building = committed.name, session = session,
        }) then
            session.stock = nil
            session.reading = false
            session.warehouse = "plugin is busy; refresh the overview"
            S.view_present(session)
        end
        return
    end
    S.view_bind(session, building)
end

S.view_bind = function(session: any, building: any)
    if building.structure_id == DONE then
        session.stock = nil
        session.reading = false
        session.warehouse = "the committed warehouse stored no core structure id"
        S.view_present(session)
        return
    end
    if S.requests_pending() >= MAX_REQUESTS then
        session.stock = nil
        session.reading = false
        session.warehouse = "plugin is busy; refresh the overview"
        S.view_present(session)
        return
    end
    local request = S.begin_request("view-bind", { id = session.id, view = session.request_id })
    -- Core answers a repeated bind of the same authored container with the
    -- original handle, and the id below names exactly that binding, so a retry
    -- replays the receipt instead of conflicting with it.
    solaris.bind_warehouse(
        request.request_id,
        "warehouse-" .. S.sanitize_id(session.id) .. "-" .. S.view_digest(building.structure_id),
        building.structure_id,
        0
    )
end

S.view_query = function(session: any, handle: string)
    if S.requests_pending() >= MAX_REQUESTS then
        session.stock = nil
        session.reading = false
        session.warehouse = "plugin is busy; refresh the overview"
        S.view_present(session)
        return
    end
    local request = S.begin_request("view-stock", {
        id = session.id, view = session.request_id, purpose = "overview",
    })
    solaris.query_owned_inventory(request.request_id, { kind = "warehouse", handle = handle }, nil)
end

-- Open the declared screen. The first model carries only state the plugin
-- already holds, so the screen appears immediately; the warehouse read then
-- presents the same page again with the confirmed contents.
S.view_begin = function(player_id: number, uuid: string, id: string, page: number): boolean
    local record = settlements[id]
    if record == nil or not S.is_member(record, uuid) then return false end
    if S.requests_pending() >= MAX_REQUESTS then return false end
    local session: any = {
        id = id,
        uuid = uuid,
        player_id = player_id,
        page = page,
        instance = nil,
        revision = 0,
        reading = false,
        dirty = false,
        stock = nil,
        warehouse = "reading the bound warehouse",
    }
    local request = S.begin_request("view-open", { id = id, player_id = player_id })
    session.request_id = request.request_id
    session.open_entry = request
    view_opens[session.request_id] = session
    S.view_remember(session)
    solaris.open_client_view(session.request_id, player_id, VIEW_ID, S.view_model(session))
    S.view_read_warehouse(session)
    return true
end

S.view_open_for = function(player_id: number, players: any)
    local uuid = S.view_player_uuid(player_id, players)
    if uuid == nil then
        S.message(player_id, "Your player identity is unavailable; retry the settlement screen.")
        return
    end
    local record, count = S.view_only_settlement(uuid)
    if record == nil then
        if count > 1 then
            S.message(player_id, "You belong to several settlements; use /settlement overview <name>.")
        else
            S.message(player_id, "You are not a member of any loaded settlement.")
        end
        return
    end
    S.view_begin(player_id, uuid, record.name, 0)
end

function on_loader_view_request(event: any)
    if event.request_kind ~= "settlement" then return end
    local player_id = event.player_id
    if not boot_done then
        S.message(player_id, "Settlements are still loading.")
        return
    end
    if S.requests_pending() >= MAX_REQUESTS then
        S.message(player_id, "Plugin is busy; reopen the settlement screen.")
        return
    end
    local entry = S.begin_request("view-request", { player_id = player_id })
    solaris.list_online_players(entry.request_id, 32)
end

function on_loader_view_action(event: any)
    local session: any = view_sessions[event.view_instance_id]
    if session == nil then return end
    -- The client reports the revision it displays, which is the only
    -- authoritative revision this plugin sees after an open.
    session.revision = event.view_revision
    if event.player_id ~= session.player_id then return end
    if settlements[session.id] == nil then return end
    local action = event.action_id
    if action == VIEW_REFRESH then
        -- The model denies this action while a read is in flight; a client that
        -- sends it anyway is answered with the current page, never silently.
        if session.reading then
            S.view_present(session)
            return
        end
        S.view_read_warehouse(session)
        return
    end
    if action == VIEW_NEXT or action == VIEW_PREV then
        if action == VIEW_NEXT then
            session.page = session.page + 1
        else
            session.page = session.page - 1
        end
        S.view_present(session)
    end
end

function on_client_view_opened(event: any)
    local session: any = view_opens[event.request_id]
    if session == nil then return end
    if not event.opened then
        S.view_forget(session)
        S.message(event.player_id, "Settlement overview refused: "
            .. tostring(event.failure or "refused") .. ".")
        return
    end
    session.instance = event.view_instance_id
    session.revision = event.revision
    view_sessions[session.instance] = session
    S.finish_request(session.open_entry)
    if session.dirty then S.view_present(session) end
end

-- ---------------------------------------------------------------------------
-- Lifecycle
-- ---------------------------------------------------------------------------

function on_server_started(_event: any)
    S.read_key(INDEX_KEY, "boot", {})
end

function on_player_command(event: any)
    if event.root ~= "settlement" then return end
    S.dispatch_command(event)
end

function on_plugin_timer(event: any)
    local timer_id = event.timer_id
    if type(timer_id) ~= "string" or string.sub(timer_id, 1, 6) ~= "cycle-" then return end
    local id = nil
    for settlement_id, registered in pairs(cycle_timers) do
        if registered == timer_id then
            id = settlement_id
            break
        end
    end
    if id == nil then return end
    cycle_timers[id] = nil
    S.run_cycle(id, event.fired_tick)
end

function on_command_batch_rejected(_result: any)
    -- The whole batch was rejected before any effect: drop the in-flight
    -- bookkeeping. Durable intents stay in storage and are resolved by
    -- operation_status on the next startup; this callback must not emit a
    -- command. No read can still be in flight, so every live screen becomes
    -- refreshable again and keeps the page it last presented.
    requests = {}
    for position = 1, #view_order do
        view_order[position].reading = false
    end
end

return nil
