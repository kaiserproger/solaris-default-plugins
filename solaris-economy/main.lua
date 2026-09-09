--!strict

local config: any = solaris.config()
local storage_key = "ledger-v1"
local accounts: any = {}
local tokens: any = {}
local token_order: { string } = {}
local storage_version: any = nil
local loaded = false
local pending: any = nil
local sequence = 0

assert(type(config.currency_name) == "string" and #config.currency_name >= 1 and #config.currency_name <= 32)
assert(type(config.starting_balance) == "number" and config.starting_balance % 1 == 0 and config.starting_balance >= 0)
assert(type(config.maximum_balance) == "number" and config.maximum_balance % 1 == 0)
assert(config.maximum_balance >= config.starting_balance and config.maximum_balance <= 1000000000)
assert(type(config.maximum_accounts) == "number" and config.maximum_accounts % 1 == 0)
assert(config.maximum_accounts >= 2 and config.maximum_accounts <= 48)
assert(type(config.maximum_transfer_tokens) == "number" and config.maximum_transfer_tokens % 1 == 0)
assert(config.maximum_transfer_tokens >= 1 and config.maximum_transfer_tokens <= 32)

local function normalize_uuid(value: string): string?
    local without_hyphens = string.gsub(value, "-", "")
    local normalized = string.lower(without_hyphens)
    if #normalized ~= 32 or string.match(normalized, "^[0-9a-f]+$") == nil then return nil end
    return normalized
end

local function valid_token(value: string): boolean
    return #value >= 1 and #value <= 20 and string.match(value, "^[a-z0-9_-]+$") ~= nil
end

local function count(values: any): number
    local result = 0
    for _ in pairs(values) do result = result + 1 end
    return result
end

local function next_request(): string
    sequence = sequence + 1
    return "ledger-" .. tostring(sequence)
end

local function balance(values: any, uuid: string): number
    local value = values[uuid]
    return value == nil and config.starting_balance or value
end

local function copy_accounts(): any
    local copied: any = {}
    for uuid, amount in pairs(accounts) do copied[uuid] = amount end
    return copied
end

local function copy_token_order(): { string }
    local copied: { string } = {}
    for _, key in ipairs(token_order) do copied[#copied + 1] = key end
    return copied
end

local function encode(next_accounts: any, next_tokens: { string }): string
    local ids: { string } = {}
    for uuid in pairs(next_accounts) do ids[#ids + 1] = uuid end
    table.sort(ids)
    local account_rows: { string } = {}
    for _, uuid in ipairs(ids) do account_rows[#account_rows + 1] = uuid .. "," .. tostring(next_accounts[uuid]) end
    return "v1|" .. table.concat(account_rows, ";") .. "|" .. table.concat(next_tokens, ";")
end

local function decode(value: any): (any, any, any)
    if value == nil then return {}, {}, {} end
    if type(value) ~= "string" then return nil, nil, nil end
    local account_text, token_text = string.match(value, "^v1|([^|]*)|([^|]*)$")
    if account_text == nil then return nil, nil, nil end
    if token_text == nil then return nil, nil, nil end
    local decoded_accounts: any = {}
    if account_text ~= "" then
        for row in string.gmatch(account_text, "([^;]+)") do
            local uuid_text, amount_text = string.match(row, "^([0-9a-f]+),(%d+)$")
            local uuid = normalize_uuid(uuid_text or "")
            local amount = tonumber(amount_text)
            if uuid == nil or amount == nil or amount % 1 ~= 0 or amount < 0
                or amount > config.maximum_balance or decoded_accounts[uuid] ~= nil then return nil, nil, nil end
            decoded_accounts[uuid] = amount
            if count(decoded_accounts) > config.maximum_accounts then return nil, nil, nil end
        end
    end
    local decoded_tokens: any = {}
    local decoded_order: any = {}
    if token_text ~= "" then
        for key in string.gmatch(token_text, "([^;]+)") do
            if decoded_tokens[key] then return nil, nil, nil end
            decoded_tokens[key] = true
            decoded_order[#decoded_order + 1] = key
            if #decoded_order > config.maximum_transfer_tokens then return nil, nil, nil end
        end
    end
    return decoded_accounts, decoded_tokens, decoded_order
end

local function save(player_id: number, next_accounts: any, next_order: { string }, message: string)
    if pending ~= nil then
        solaris.send_message(player_id, "Another economy update is committing; retry.")
        return
    end
    local revision = storage_version == nil and "new" or tostring(storage_version)
    local request_id = "ledger-v" .. revision
    pending = { request_id = request_id, player_id = player_id, accounts = next_accounts, order = next_order, message = message }
    solaris.storage_cas(request_id, storage_key, storage_version, encode(next_accounts, next_order))
end

function on_server_started(_event: any)
    solaris.storage_get("ledger-load", storage_key)
end

function on_player_command(event: any)
    if not loaded then
        solaris.send_message(event.player_id, "Economy is still loading.")
        return
    end
    local actor = normalize_uuid(event.uuid)
    if actor == nil then return end
    local words: { string } = {}
    for word in string.gmatch(event.arguments, "%S+") do words[#words + 1] = word end

    if event.root == "money" then
        if #words ~= 0 then
            solaris.send_message(event.player_id, "Usage: /money")
        else
            solaris.send_message(event.player_id, "Balance: " .. tostring(balance(accounts, actor)) .. " " .. config.currency_name .. ".")
        end
        return
    end

    if event.root == "pay" then
        if #words ~= 3 then
            solaris.send_message(event.player_id, "Usage: /pay <player-uuid> <amount> <token>")
            return
        end
        local target = normalize_uuid(words[1])
        local amount = tonumber(words[2])
        local token = string.lower(words[3])
        if target == nil or target == actor or amount == nil or amount % 1 ~= 0 or amount < 1
            or amount > config.maximum_balance or not valid_token(token) then
            solaris.send_message(event.player_id, "Invalid recipient, amount, or token.")
            return
        end
        local token_key = actor .. ":" .. token
        if tokens[token_key] then
            solaris.send_message(event.player_id, "That transfer token was already committed.")
            return
        end
        local actor_balance = balance(accounts, actor)
        local target_balance = balance(accounts, target)
        if actor_balance < amount or target_balance + amount > config.maximum_balance then
            solaris.send_message(event.player_id, "Transfer rejected by balance limits.")
            return
        end
        local next_accounts = copy_accounts()
        if next_accounts[actor] == nil and count(next_accounts) >= config.maximum_accounts then
            solaris.send_message(event.player_id, "Account limit reached.")
            return
        end
        next_accounts[actor] = actor_balance - amount
        if next_accounts[target] == nil and count(next_accounts) >= config.maximum_accounts then
            solaris.send_message(event.player_id, "Account limit reached.")
            return
        end
        next_accounts[target] = target_balance + amount
        local next_order = copy_token_order()
        next_order[#next_order + 1] = token_key
        while #next_order > config.maximum_transfer_tokens do table.remove(next_order, 1) end
        save(event.player_id, next_accounts, next_order, "Paid " .. tostring(amount) .. " " .. config.currency_name .. ".")
        return
    end

    if event.root == "econadmin" then
        if not event.operator then
            solaris.send_message(event.player_id, "Only an operator can administer balances.")
            return
        end
        if #words ~= 3 or (words[1] ~= "set" and words[1] ~= "add") then
            solaris.send_message(event.player_id, "Usage: /econadmin <set|add> <player-uuid> <amount>")
            return
        end
        local target = normalize_uuid(words[2])
        local amount = tonumber(words[3])
        if target == nil or amount == nil or amount % 1 ~= 0 then
            solaris.send_message(event.player_id, "Invalid UUID or integer amount.")
            return
        end
        local next_amount = words[1] == "set" and amount or (balance(accounts, target) + amount)
        if next_amount < 0 or next_amount > config.maximum_balance then
            solaris.send_message(event.player_id, "Balance is outside configured limits.")
            return
        end
        local next_accounts = copy_accounts()
        if next_accounts[target] == nil and count(next_accounts) >= config.maximum_accounts then
            solaris.send_message(event.player_id, "Account limit reached.")
            return
        end
        next_accounts[target] = next_amount
        save(event.player_id, next_accounts, copy_token_order(), "Balance set to " .. tostring(next_amount) .. ".")
    end
end

function on_plugin_storage_get_result(event: any)
    if event.key ~= storage_key or event.failure ~= nil then loaded = false return end
    local decoded_accounts, decoded_tokens, decoded_order = decode(event.value)
    if decoded_accounts == nil then loaded = false return end
    accounts, tokens, token_order = decoded_accounts, decoded_tokens, decoded_order
    storage_version = event.version
    loaded = true
end

function on_plugin_storage_cas_result(event: any)
    local current = pending
    if current == nil or current.request_id ~= event.request_id then return end
    pending = nil
    if event.failure ~= nil or not event.applied then
        loaded = false
        solaris.send_message(current.player_id, "Economy ledger changed; retry after reload.")
        solaris.storage_get("ledger-reload-" .. tostring(sequence), storage_key)
        return
    end
    accounts = current.accounts
    token_order = current.order
    tokens = {}
    for _, key in ipairs(token_order) do tokens[key] = true end
    storage_version = event.version
    solaris.send_message(current.player_id, current.message)
end

function on_command_batch_rejected(_result: any)
    pending = nil
end
