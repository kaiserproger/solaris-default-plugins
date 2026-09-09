--!strict

-- Displays a fresh server-authoritative player list in an inventory menu.

local query_limit = 54
local next_request = 0
local pending_by_request: any = {}
local pending_by_player: any = {}
local last_batch: any = nil

local function clear_player(player_id: number)
    local request_id = pending_by_player[player_id]
    if request_id ~= nil then
        pending_by_player[player_id] = nil
        pending_by_request[request_id] = nil
    end
end

function on_player_command(event: any)
    last_batch = nil
    if event.root ~= "who" or pending_by_player[event.player_id] ~= nil then
        return
    end
    next_request = next_request + 1
    local request_id = "who-" .. tostring(event.player_id) .. "-" .. tostring(next_request)
    pending_by_request[request_id] = {
        player_id = event.player_id,
        menu_id = "online-roster-" .. tostring(next_request),
    }
    pending_by_player[event.player_id] = request_id
    last_batch = {
        kind = "query",
        request_id = request_id,
        player_id = event.player_id,
    }
    solaris.list_online_players(request_id, query_limit)
end

function on_player_online_result(event: any)
    last_batch = nil
    local pending = pending_by_request[event.request_id]
    if pending == nil then
        return
    end
    local player_id = pending.player_id
    pending_by_request[event.request_id] = nil
    pending_by_player[player_id] = nil

    local slots: any = {}
    for index, raw_player in ipairs(event.players) do
        local player: any = raw_player
        local dimension_limit = 128 - #player.username - 3
        slots[index] = {
            slot = index - 1,
            resource = "minecraft:paper",
            count = 1,
            label = player.username .. " | " .. string.sub(player.dimension, 1, dimension_limit),
        }
    end
    last_batch = { kind = "open" }
    solaris.open_inventory_menu(
        player_id,
        pending.menu_id,
        "Online Players (" .. tostring(#event.players) .. ")",
        slots
    )
    if event.truncated then
        solaris.send_message(player_id, "Online list truncated to 54 players.")
    end
end

function on_player_left(event: any)
    last_batch = nil
    clear_player(event.player_id)
end

function on_command_batch_rejected(_result: any)
    local batch = last_batch
    last_batch = nil
    if batch ~= nil and batch.kind == "query" then
        pending_by_request[batch.request_id] = nil
        pending_by_player[batch.player_id] = nil
    end
end
