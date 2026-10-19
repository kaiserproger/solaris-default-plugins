//! Offline-mode session authentication. Never log command arguments/passwords.
use std::collections::BTreeMap;

use solaris_plugin_sdk::events::{CommandInvoked, Event};
use solaris_plugin_sdk::{
    cancel_timer, disconnect_player, export_plugin, message_session, password_hash,
    password_verify, schedule_timer, storage_cas, storage_get, unlock_session, Command, Config,
    EventContext, Failure, InitContext, Plugin, StorageCasOutcome, StorageGetOutcome,
};

const TIMEOUT_TICKS: u64 = 1_200;
const PROMPT_REGISTER: &str =
    "Зарегистрируйтесь /register <пароль> <пароль> / Register: /register <password> <password>";
const PROMPT_LOGIN: &str = "Войдите /login <пароль> / Log in: /login <password>";

struct Account {
    hash: String,
    version: u64,
}

#[derive(Default)]
struct Session {
    player: String,
    key: String,
    loaded: bool,
    account: Option<Account>,
    authenticated: bool,
    failures: u8,
    busy: bool,
}

struct Save {
    session: u64,
    hash: String,
    change: bool,
}

#[derive(Default)]
struct Auth {
    sessions: BTreeMap<u64, Session>,
    reads: BTreeMap<String, u64>,
    saves: BTreeMap<String, Save>,
    sequence: u64,
}

impl Auth {
    fn request(&mut self, prefix: &str) -> String {
        self.sequence += 1;
        format!("{prefix}-{}", self.sequence)
    }

    fn read(&mut self, session: u64) -> Command {
        let request = self.request("read");
        self.reads.insert(request.clone(), session);
        storage_get(&request, &self.sessions[&session].key)
    }

    fn authenticate(&mut self, session: u64) -> Vec<Command> {
        let state = self.sessions.get_mut(&session).expect("live session");
        state.authenticated = true;
        state.busy = false;
        state.failures = 0;
        vec![
            unlock_session(session),
            cancel_timer(&format!("auth-{session}")),
            message_session(session, "Вход выполнен / Successfully logged in"),
        ]
    }

    fn wrong(&mut self, session: u64) -> Vec<Command> {
        let state = self.sessions.get_mut(&session).expect("live session");
        state.failures += 1;
        if state.failures >= 5 {
            state.authenticated = false;
            state.busy = true;
            vec![disconnect_player(
                session,
                "Неверный пароль 5 раз / Wrong password 5 times",
            )]
        } else {
            vec![message_session(session, "Неверный пароль / Wrong password")]
        }
    }

    fn save(&mut self, session: u64, password: &str, change: bool) -> Vec<Command> {
        let hash = password_hash(password);
        if !hash.starts_with("$argon2id$") {
            return vec![disconnect_player(
                session,
                "Ошибка хеширования / Password hashing unavailable",
            )];
        }
        let request = self.request("save");
        let state = self.sessions.get_mut(&session).expect("live session");
        state.busy = true;
        let command = storage_cas(
            &request,
            &state.key,
            state.account.as_ref().map(|account| account.version),
            &hash,
        );
        self.saves.insert(
            request,
            Save {
                session,
                hash,
                change,
            },
        );
        vec![command]
    }

    fn command(&mut self, invoked: &CommandInvoked) -> Vec<Command> {
        let session = invoked.session;
        let Some(state) = self.sessions.get(&session) else {
            return vec![disconnect_player(
                session,
                "Переподключитесь / Reconnect to authenticate",
            )];
        };
        if state.player != invoked.player
            || !state.key["account:".len()..].eq_ignore_ascii_case(&invoked.username)
        {
            return vec![disconnect_player(
                session,
                "Несовпадение сессии / Session mismatch",
            )];
        }
        if !state.loaded || state.busy {
            return vec![message_session(session, "Подождите / Please wait")];
        }
        let args = &invoked.arguments;
        match invoked.name.as_str() {
            "register" => {
                if state.account.is_some() {
                    return vec![message_session(session, PROMPT_LOGIN)];
                }
                if args.len() != 2 || args[0] != args[1] || !valid_password(&args[0]) {
                    return vec![message_session(session, "Пароли должны совпадать (4–128 байт) / Matching passwords required (4–128 bytes): /register <password> <password>")];
                }
                self.save(session, &args[0], false)
            }
            "login" => {
                if state.authenticated {
                    return vec![message_session(session, "Вы уже вошли / Already logged in")];
                }
                let Some(account) = &state.account else {
                    return vec![message_session(session, PROMPT_REGISTER)];
                };
                if args.len() != 1 {
                    return vec![message_session(
                        session,
                        "Используйте / Usage: /login <password>",
                    )];
                }
                if args[0].len() > 128 || !password_verify(&args[0], &account.hash) {
                    return self.wrong(session);
                }
                self.authenticate(session)
            }
            "changepassword" => {
                if !state.authenticated {
                    return vec![message_session(session, PROMPT_LOGIN)];
                }
                if args.len() != 2 || !valid_password(&args[1]) {
                    return vec![message_session(
                        session,
                        "Используйте / Usage: /changepassword <old> <new> (4–128 bytes)",
                    )];
                }
                let account = state.account.as_ref().expect("authenticated account");
                if args[0].len() > 128 || !password_verify(&args[0], &account.hash) {
                    return self.wrong(session);
                }
                self.save(session, &args[1], true)
            }
            _ => Vec::new(),
        }
    }
}

fn valid_password(password: &str) -> bool {
    (4..=128).contains(&password.len())
}

impl Plugin for Auth {
    fn init(&mut self, _config: &Config, _context: &InitContext) -> Result<Vec<Command>, Failure> {
        *self = Self::default();
        Ok(Vec::new())
    }

    fn on_events(
        &mut self,
        _context: &EventContext,
        events: &[Event],
    ) -> Result<Vec<Command>, Failure> {
        let mut commands = Vec::new();
        for event in events {
            match event {
                Event::PlayerJoined(joined) => {
                    self.sessions.insert(
                        joined.session,
                        Session {
                            player: joined.player.clone(),
                            key: format!("account:{}", joined.name.to_ascii_lowercase()),
                            ..Session::default()
                        },
                    );
                    commands.push(schedule_timer(
                        &format!("auth-{}", joined.session),
                        TIMEOUT_TICKS,
                    ));
                    commands.push(self.read(joined.session));
                }
                Event::PlayerLeft(left) => {
                    self.sessions.remove(&left.session);
                    self.reads.retain(|_, session| *session != left.session);
                    self.saves.retain(|_, save| save.session != left.session);
                    commands.push(cancel_timer(&format!("auth-{}", left.session)));
                }
                Event::CommandInvoked(invoked) => commands.extend(self.command(invoked)),
                Event::StorageGetAnswered(answer) => {
                    let Some(session) = self.reads.remove(&answer.request) else {
                        continue;
                    };
                    let Some(state) = self.sessions.get_mut(&session) else {
                        continue;
                    };
                    match &answer.outcome {
                        StorageGetOutcome::Read(record) => {
                            state.account = match (&record.value, record.version) {
                                (Some(hash), Some(version)) if hash.starts_with("$argon2id$") => {
                                    Some(Account {
                                        hash: hash.clone(),
                                        version,
                                    })
                                }
                                (None, None) => None,
                                _ => {
                                    commands.push(disconnect_player(
                                        session,
                                        "Ошибка записи аккаунта / Invalid account record",
                                    ));
                                    continue;
                                }
                            };
                            state.loaded = true;
                            state.busy = false;
                            commands.push(message_session(
                                session,
                                if state.account.is_some() {
                                    PROMPT_LOGIN
                                } else {
                                    PROMPT_REGISTER
                                },
                            ));
                        }
                        StorageGetOutcome::Failed(_) => commands.push(disconnect_player(
                            session,
                            "Хранилище недоступно / Authentication storage unavailable",
                        )),
                    }
                }
                Event::StorageCasAnswered(answer) => {
                    let Some(save) = self.saves.remove(&answer.request) else {
                        continue;
                    };
                    let session = save.session;
                    let Some(state) = self.sessions.get_mut(&session) else {
                        continue;
                    };
                    match answer.outcome {
                        StorageCasOutcome::Committed(version) => {
                            state.account = Some(Account {
                                hash: save.hash,
                                version,
                            });
                            if save.change {
                                state.busy = false;
                                state.failures = 0;
                                commands.push(message_session(
                                    session,
                                    "Пароль изменён / Password changed",
                                ));
                            } else {
                                commands.push(message_session(
                                    session,
                                    "Аккаунт создан / Successfully registered",
                                ));
                                commands.extend(self.authenticate(session));
                            }
                        }
                        StorageCasOutcome::Refused => {
                            state.busy = true;
                            state.loaded = false;
                            commands.push(message_session(session, "Запись не сохранена, повторите / Account changed; retry after prompt"));
                            commands.push(self.read(session));
                        }
                        StorageCasOutcome::Failed(_) => commands.push(disconnect_player(
                            session,
                            "Хранилище недоступно / Authentication storage unavailable",
                        )),
                    }
                }
                Event::TimerFired(timer) => {
                    if let Some(session) = timer
                        .timer_id
                        .strip_prefix("auth-")
                        .and_then(|value| value.parse::<u64>().ok())
                    {
                        if let Some(state) = self.sessions.get_mut(&session) {
                            if !state.authenticated {
                                // Ignore any storage callback arriving after the disconnect decision.
                                self.reads.retain(|_, value| *value != session);
                                self.saves.retain(|_, save| save.session != session);
                                state.busy = true;
                                commands.push(disconnect_player(
                                    session,
                                    "Время входа истекло / Authentication timeout (60 seconds)",
                                ));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(commands)
    }
}

export_plugin!(Auth);
