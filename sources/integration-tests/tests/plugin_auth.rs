//! Focused component acceptance for the `solaris-auth` package.
//!
//! Mirrors `plugin_standard_pack.rs`'s Wasmtime instance setup, but drives the
//! authentication guest directly: join arms one `auth-<session>` timeout and
//! loads `account:<lowercase name>`; register hashes then emits the storage swap
//! immediately; the committed swap alone authenticates (register commit also
//! unlocks and cancels the timeout); login verifies then unlocks immediately;
//! the timeout gates the pending session. The native fail-closed session gate
//! is the only enforcement path; the guest declares no build hook.

use mc_plugin_host::bindings::exports::solaris::plugin::events::{
    CommandInvoked, Event, EventContext, PlayerJoined, PlayerLeft, StorageCasAnswered,
    StorageGetAnswered, TimerFired,
};
use mc_plugin_host::bindings::exports::solaris::plugin::lifecycle::InitContext;
use mc_plugin_host::bindings::solaris::plugin::commands::{Command, MessageTarget};
use mc_plugin_host::bindings::solaris::plugin::storage::{
    StorageCasOutcome, StorageGetOutcome, StorageRecord,
};
use mc_plugin_host::bindings::solaris::plugin::types::Position;
use mc_plugin_host::{
    engine, linker, CompiledPlugin, HostServices, LogLevel, PluginInstance, PluginLimits,
    PluginStartup,
};

const API_VERSION: &str = "0.8.0";
const PLAYER: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const SESSION: u64 = 7;
const USERNAME: &str = "Ada";
const STORAGE_KEY: &str = "account:ada";
const AUTH_TIMER: &str = "auth-7";
const PASSWORD: &str = "AuthCheck9";
const NEW_PASSWORD: &str = "NewCheck99";
const WRONG_PASSWORD: &str = "Wrong0000";
const AUTH_TIMEOUT_TICKS: u64 = 1_200;

#[derive(Default)]
struct Services {
    id: String,
}

impl HostServices for Services {
    fn log(&mut self, _level: LogLevel, _message: &str) {
        panic!("authentication plugin must not write logs");
    }

    fn plugin_id(&self) -> &str {
        &self.id
    }
}

fn plugins_root() -> std::path::PathBuf {
    if let Some(root) = std::env::var_os("SOLARIS_DEFAULT_PLUGINS_ROOT") {
        return std::path::PathBuf::from(root);
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("integration test crate belongs to the plugin repository")
        .to_path_buf()
}

/// Instantiate `id` with an empty config: the rewritten guest takes no
/// configuration (`password_hash`/`password_verify` are host imports).
fn instance(id: &str) -> (PluginInstance<Services>, mc_plugin_host::EpochTicker) {
    let package = plugins_root().join(id);
    let component =
        std::fs::read(package.join("plugin.wasm")).expect("solaris-auth component is installed");
    let limits = PluginLimits::default();
    let engine = engine(&limits).expect("host engine builds");
    let compiled = CompiledPlugin::compile(&engine, &component, &limits, API_VERSION)
        .expect("solaris-auth compiles under the current host contract");
    let ticker =
        mc_plugin_host::EpochTicker::start(engine.clone(), std::time::Duration::from_millis(25))
            .expect("real callback watchdog starts");
    let linker = linker::<Services>(&engine).expect("host linker builds");
    let mut startup = PluginStartup::instantiate(
        &linker,
        compiled.component(),
        Services { id: id.to_owned() },
        limits,
    )
    .expect("startup instance builds");
    assert!(
        startup
            .configure("")
            .expect("solaris-auth configuration is accepted")
            .is_none(),
        "auth declares no startup contribution"
    );
    let mut plugin = PluginInstance::instantiate(
        &linker,
        compiled.component(),
        Services { id: id.to_owned() },
        limits,
    )
    .expect("runtime instance builds");
    assert!(
        plugin
            .init(
                "",
                InitContext {
                    plugin_id: id.to_owned(),
                    api_version: API_VERSION.to_owned(),
                    world_fingerprint: String::new(),
                },
            )
            .expect("solaris-auth init is accepted")
            .into_commands()
            .is_empty(),
        "auth init emits no commands"
    );
    (plugin, ticker)
}

fn context(count: u32) -> EventContext {
    EventContext {
        tick: 42,
        first_sequence: 0,
        count,
    }
}

fn joined() -> Event {
    Event::PlayerJoined(PlayerJoined {
        player: PLAYER.to_owned(),
        session: SESSION,
        name: USERNAME.to_owned(),
    })
}

fn left() -> Event {
    Event::PlayerLeft(PlayerLeft { session: SESSION })
}

fn auth_command(name: &str, arguments: &[&str], raw_arguments: &str) -> Event {
    Event::CommandInvoked(CommandInvoked {
        player: PLAYER.to_owned(),
        session: SESSION,
        username: USERNAME.to_owned(),
        operator: false,
        position: Position {
            x: 32.0,
            y: 64.0,
            z: -16.0,
        },
        dimension: "minecraft:overworld".to_owned(),
        name: name.to_owned(),
        arguments: arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect(),
        raw_arguments: raw_arguments.to_owned(),
    })
}

fn auth_timeout_fired() -> Event {
    Event::TimerFired(TimerFired {
        timer_id: AUTH_TIMER.to_owned(),
        scheduled_tick: 42 + AUTH_TIMEOUT_TICKS,
        fired_tick: 42 + AUTH_TIMEOUT_TICKS,
    })
}

fn storage_empty(request: String) -> Event {
    Event::StorageGetAnswered(StorageGetAnswered {
        request,
        outcome: StorageGetOutcome::Read(StorageRecord {
            value: None,
            version: None,
        }),
    })
}

fn storage_value(request: String, value: &str, version: u64) -> Event {
    Event::StorageGetAnswered(StorageGetAnswered {
        request,
        outcome: StorageGetOutcome::Read(StorageRecord {
            value: Some(value.to_owned()),
            version: Some(version),
        }),
    })
}

fn cas_committed(request: String, version: u64) -> Event {
    Event::StorageCasAnswered(StorageCasAnswered {
        request,
        outcome: StorageCasOutcome::Committed(version),
    })
}

struct Cas {
    request: String,
    key: String,
    expected: Option<u64>,
    value: String,
}

fn take_cas(commands: &[Command]) -> Cas {
    assert_eq!(
        commands.len(),
        1,
        "register hashes then emits exactly the storage swap"
    );
    match &commands[0] {
        Command::StorageCas(cas) => Cas {
            request: cas.request.clone(),
            key: cas.key.clone(),
            expected: cas.expected_version,
            value: cas.value.clone(),
        },
        other => panic!("expected a storage compare-and-swap, saw {other:?}"),
    }
}

fn session_text(commands: &[Command]) -> Vec<String> {
    commands
        .iter()
        .filter_map(|command| match command {
            Command::SendMessage(message) => match &message.target {
                MessageTarget::Session(session) => {
                    assert_eq!(*session, SESSION, "auth replies target the session");
                    for password in [PASSWORD, NEW_PASSWORD, WRONG_PASSWORD] {
                        assert!(
                            !message.text.contains(password),
                            "password leaked into a reply"
                        );
                    }
                    Some(message.text.clone())
                }
                _ => panic!("authentication replies must address the exact session"),
            },
            Command::Broadcast(_) => panic!("authentication must never broadcast"),
            _ => None,
        })
        .collect()
}

fn has_command<F: Fn(&Command) -> bool>(commands: &[Command], want: F) -> bool {
    commands.iter().any(want)
}

fn is_unlock(command: &Command) -> bool {
    matches!(command, Command::UnlockSession(unlock) if unlock.session == SESSION)
}

fn cancels(commands: &[Command]) -> Vec<String> {
    commands
        .iter()
        .filter_map(|command| match command {
            Command::CancelTimer(timer) => Some(timer.timer_id.clone()),
            _ => None,
        })
        .collect()
}

fn disconnect_reason(commands: &[Command]) -> String {
    let mut reasons = Vec::new();
    for command in commands {
        if let Command::DisconnectPlayer(disconnect) = command {
            assert_eq!(disconnect.session, SESSION, "auth kicks the session");
            reasons.push(disconnect.reason.clone());
        }
    }
    assert_eq!(reasons.len(), 1, "exactly one disconnect is expected");
    reasons.pop().expect("one disconnect")
}

/// Join arms the auth timeout and loads the lowercase account key.
fn join_and_read(plugin: &mut PluginInstance<Services>) -> String {
    let commands = plugin
        .on_events(context(1), &[joined()])
        .expect("join delivery succeeds")
        .into_commands();
    assert_eq!(
        commands.len(),
        2,
        "join arms the auth timeout and loads the account"
    );
    let mut read = None;
    for command in &commands {
        match command {
            Command::ScheduleTimer(timer) => {
                assert_eq!(
                    timer.timer_id, AUTH_TIMER,
                    "auth timeout owns auth-<session>"
                );
                assert_eq!(
                    timer.delay_ticks, AUTH_TIMEOUT_TICKS,
                    "auth timeout fires at 1200 ticks"
                );
            }
            Command::StorageGet(get) => {
                assert_eq!(get.key, STORAGE_KEY, "storage key lowercases the username");
                read = Some(get.request.clone());
            }
            other => panic!("join emits only the auth timer and the account read, saw {other:?}"),
        }
    }
    read.expect("join loads the account")
}

/// Full register with the `AuthCheck9` password through the committed outcome.
/// The swap value is the stored PHC directly (`$argon2id$…`). Returns it for
/// relog replays.
fn commit_register(plugin: &mut PluginInstance<Services>) -> String {
    let read = join_and_read(plugin);
    let prompt = plugin
        .on_events(context(1), &[storage_empty(read)])
        .expect("empty storage load succeeds")
        .into_commands();
    assert!(
        session_text(&prompt)
            .iter()
            .any(|text| text.contains("Register")),
        "an unknown account is prompted to register"
    );
    let saved = plugin
        .on_events(
            context(1),
            &[auth_command(
                "register",
                &[PASSWORD, PASSWORD],
                "AuthCheck9 AuthCheck9",
            )],
        )
        .expect("register hashes and saves")
        .into_commands();
    let cas = take_cas(&saved);
    assert_eq!(
        cas.key, STORAGE_KEY,
        "register stores under the lowercase key"
    );
    assert_eq!(
        cas.expected, None,
        "a fresh account swaps only if the key holds nothing"
    );
    assert!(
        cas.value.starts_with("$argon2id$"),
        "the stored value is the native PHC hash directly"
    );
    let committed = plugin
        .on_events(context(1), &[cas_committed(cas.request, 1)])
        .expect("register commit succeeds")
        .into_commands();
    let texts = session_text(&committed);
    assert!(
        texts.iter().any(|text| text.contains("registered")),
        "the committed register is announced before authenticating"
    );
    assert!(
        texts.iter().any(|text| text.contains("logged in")),
        "the committed register logs the session in"
    );
    assert!(
        cancels(&committed).contains(&AUTH_TIMER.to_owned()),
        "login cancels the auth timeout"
    );
    assert!(
        has_command(&committed, is_unlock),
        "register commit unlocks the session only after the CAS committed"
    );
    assert_eq!(
        committed.len(),
        4,
        "register commit authenticates: registered reply, unlock_session, timeout cancel, login reply"
    );
    cas.value
}

#[test]
fn join_arms_timeout_and_loads_lowercase_key() {
    let (mut plugin, _ticker) = instance("solaris-auth");
    join_and_read(&mut plugin);
    plugin.shutdown().expect("clean component shutdown");
}

#[test]
fn register_hashes_then_cas_before_authenticating() {
    let (mut plugin, _ticker) = instance("solaris-auth");
    commit_register(&mut plugin);
    plugin.shutdown().expect("clean component shutdown");
}

#[test]
fn relog_login_authenticates_with_committed_value() {
    let (mut plugin, _ticker) = instance("solaris-auth");
    let value = commit_register(&mut plugin);
    let gone = plugin
        .on_events(context(1), &[left()])
        .expect("leave delivery succeeds")
        .into_commands();
    assert!(
        cancels(&gone).contains(&AUTH_TIMER.to_owned()),
        "leave drops the auth timeout"
    );
    let read = join_and_read(&mut plugin);
    let prompt = plugin
        .on_events(context(1), &[storage_value(read, &value, 1)])
        .expect("stored account load succeeds")
        .into_commands();
    assert!(
        session_text(&prompt)
            .iter()
            .any(|text| text.contains("Log in")),
        "a known account is prompted to log in"
    );
    let done = plugin
        .on_events(context(1), &[auth_command("login", &[PASSWORD], PASSWORD)])
        .expect("login verifies then unlocks")
        .into_commands();
    let texts = session_text(&done);
    assert!(
        texts.iter().any(|text| text.contains("logged in")),
        "the verified login logs the session in"
    );
    assert!(
        cancels(&done).contains(&AUTH_TIMER.to_owned()),
        "login cancels the auth timeout"
    );
    assert!(
        has_command(&done, is_unlock),
        "verified login unlocks the session immediately"
    );
    assert_eq!(
        done.len(),
        3,
        "verified login authenticates: unlock_session, timeout cancel, login reply"
    );
    plugin.shutdown().expect("clean component shutdown");
}

#[test]
fn wrong_password_kick_at_five() {
    let (mut plugin, _ticker) = instance("solaris-auth");
    let value = commit_register(&mut plugin);
    let _ = plugin
        .on_events(context(1), &[left()])
        .expect("leave delivery succeeds");
    let read = join_and_read(&mut plugin);
    let _ = plugin
        .on_events(context(1), &[storage_value(read, &value, 1)])
        .expect("stored account load succeeds");
    for attempt in 1..=5 {
        let done = plugin
            .on_events(
                context(1),
                &[auth_command("login", &[WRONG_PASSWORD], WRONG_PASSWORD)],
            )
            .expect("wrong login answers")
            .into_commands();
        if attempt < 5 {
            let texts = session_text(&done);
            assert!(
                texts.iter().any(|text| text.contains("Wrong password")),
                "wrong attempt {attempt} warns without kicking"
            );
            assert!(
                !has_command(&done, |command| matches!(
                    command,
                    Command::DisconnectPlayer(_)
                )),
                "wrong attempt {attempt} must not disconnect"
            );
            assert!(
                !has_command(&done, is_unlock),
                "a wrong password must never unlock the session"
            );
        } else {
            assert!(
                disconnect_reason(&done).contains("Wrong password 5 times"),
                "the fifth wrong password kicks the session"
            );
        }
    }
    plugin.shutdown().expect("clean component shutdown");
}

#[test]
fn auth_timeout_disconnects() {
    let (mut plugin, _ticker) = instance("solaris-auth");
    join_and_read(&mut plugin);
    let done = plugin
        .on_events(
            EventContext {
                tick: 42 + AUTH_TIMEOUT_TICKS,
                ..context(1)
            },
            &[auth_timeout_fired()],
        )
        .expect("timeout delivery succeeds")
        .into_commands();
    assert!(
        disconnect_reason(&done).contains("Authentication timeout"),
        "the timeout disconnects the pending session"
    );
    assert!(
        !has_command(&done, is_unlock),
        "a timed-out session is never unlocked"
    );
    plugin.shutdown().expect("clean component shutdown");
}

#[test]
fn pending_commands_wait_for_load() {
    let (mut plugin, _ticker) = instance("solaris-auth");
    let read = join_and_read(&mut plugin);
    let early = plugin
        .on_events(
            context(1),
            &[auth_command(
                "register",
                &[PASSWORD, PASSWORD],
                "AuthCheck9 AuthCheck9",
            )],
        )
        .expect("pre-load command delivery succeeds")
        .into_commands();
    assert!(
        session_text(&early)
            .iter()
            .any(|text| text.contains("Please wait")),
        "commands before the account loads are asked to wait"
    );
    let _ = plugin
        .on_events(context(1), &[storage_empty(read)])
        .expect("empty storage load succeeds");
    plugin.shutdown().expect("clean component shutdown");
}

#[test]
fn changepassword_rekeys_and_relogin_uses_new() {
    let (mut plugin, _ticker) = instance("solaris-auth");
    let _ = commit_register(&mut plugin);
    let saved = plugin
        .on_events(
            context(1),
            &[auth_command(
                "changepassword",
                &[PASSWORD, NEW_PASSWORD],
                "AuthCheck9 NewCheck99",
            )],
        )
        .expect("password change verifies then saves")
        .into_commands();
    let cas = take_cas(&saved);
    assert_eq!(
        cas.key, STORAGE_KEY,
        "password change keeps the lowercase key"
    );
    assert_eq!(
        cas.expected,
        Some(1),
        "password change swaps against the committed revision"
    );
    assert!(
        cas.value.starts_with("$argon2id$"),
        "the rekeyed value is the native PHC hash directly"
    );
    let changed = plugin
        .on_events(context(1), &[cas_committed(cas.request, 2)])
        .expect("password change commit succeeds")
        .into_commands();
    assert!(
        session_text(&changed)
            .iter()
            .any(|text| text.contains("Password changed")),
        "the committed change is announced"
    );
    let _ = plugin
        .on_events(context(1), &[left()])
        .expect("leave delivery succeeds");
    let read = join_and_read(&mut plugin);
    let _ = plugin
        .on_events(context(1), &[storage_value(read, &cas.value, 2)])
        .expect("rekeyed account load succeeds");
    let old_password = plugin
        .on_events(context(1), &[auth_command("login", &[PASSWORD], PASSWORD)])
        .expect("old-password login is refused")
        .into_commands();
    assert!(session_text(&old_password)
        .iter()
        .any(|text| text.contains("Wrong password")));
    assert!(!has_command(&old_password, is_unlock));
    let verified = plugin
        .on_events(
            context(1),
            &[auth_command("login", &[NEW_PASSWORD], NEW_PASSWORD)],
        )
        .expect("new-password login verifies then unlocks")
        .into_commands();
    assert!(
        session_text(&verified)
            .iter()
            .any(|text| text.contains("logged in")),
        "the new password logs the session in"
    );
    assert!(
        has_command(&verified, is_unlock),
        "relogin with the new password unlocks the session"
    );
    plugin.shutdown().expect("clean component shutdown");
}
