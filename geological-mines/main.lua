--!strict

function on_player_command(event: any)
    if event.root == "geology" then
        solaris.send_message(
            event.player_id,
            "Realistic deposits replace vanilla ore generation in this world."
        )
    end
end
