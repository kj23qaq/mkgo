// SPDX-FileCopyrightText: 2021 Softbear, Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::admin::AdminRepo;
use crate::client::ClientRepo;
use crate::context_service::ContextService;
use crate::discord::{DiscordBotRepo, DiscordOauth2Repo};
use crate::game_service::GameArenaService;
use crate::invitation::InvitationRepo;
use crate::leaderboard::LeaderboardRepo;
use crate::metric::MetricRepo;
use crate::status::StatusRepo;
use crate::system::SystemRepo;
use actix::AsyncContext;
use actix::{Actor, Context as ActorContext, Handler, Message};
use core_protocol::id::{ArenaId, InvitationId, RegionId, ServerId};
use log::{error, info};
use minicdn::MiniCdn;
use server_util::database::Database;
use server_util::rate_limiter::RateLimiterProps;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::process;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const PRIVATE_ROOM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_PRIVATE_ROOMS: usize = 64;
const MAX_PRIVATE_ROOM_BOTS: usize = 8;

pub(crate) struct PrivateRoom<G: GameArenaService> {
    pub(crate) context_service: ContextService<G>,
    pub(crate) secret: u64,
    pub(crate) empty_since: Instant,
}

#[derive(serde::Serialize)]
pub struct PrivateRoomCreated {
    pub invitation_id: InvitationId,
    pub code: u32,
    pub secret: u64,
}

#[derive(Message)]
#[rtype(result = "Result<PrivateRoomCreated, &'static str>")]
pub struct CreatePrivateRoom {
    pub bot_count: usize,
}

#[derive(Message)]
#[rtype(result = "Result<(), &'static str>")]
pub struct SetPrivateRoomBotCount {
    pub invitation_id: InvitationId,
    pub secret: u64,
    pub bot_count: usize,
}

/// An entire game server.
pub struct Infrastructure<G: GameArenaService> {
    /// What server/region does this infrastructure represent?
    pub(crate) server_id: Option<ServerId>,
    pub(crate) region_id: Option<RegionId>,

    /// API.
    pub(crate) database: &'static Database,
    pub(crate) system: Option<SystemRepo<G>>,
    pub(crate) discord_bot: Option<&'static DiscordBotRepo>,
    pub(crate) discord_oauth2: Option<&'static DiscordOauth2Repo>,

    /// Game specific stuff. In the future, there could be multiple of these.
    pub(crate) context_service: ContextService<G>,
    pub(crate) private_rooms: HashMap<ArenaId, PrivateRoom<G>>,
    next_private_arena_id: u32,

    /// Shared invitations.
    pub(crate) invitations: InvitationRepo<G>,
    /// Shared admin interface.
    pub(crate) admin: AdminRepo<G>,
    /// Shared leaderboard.
    pub(crate) leaderboard: LeaderboardRepo<G>,
    /// Shared metrics.
    pub(crate) metrics: MetricRepo<G>,

    /// Monitoring.
    pub(crate) status: StatusRepo,

    /// Drop missed updates.
    last_update: Instant,
}

impl<G: GameArenaService> Actor for Infrastructure<G> {
    type Context = ActorContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        info!("infrastructure started");

        // TODO: Investigate whether this only affects performance or can affect correctness.
        ctx.set_mailbox_capacity(50);

        ctx.run_interval(Duration::from_secs_f32(G::TICK_PERIOD_SECS), Self::update);
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        error!("infrastructure stopped");

        // A process without this actor running should be restarted immediately.
        process::exit(1);
    }
}

impl<G: GameArenaService> Infrastructure<G> {
    pub(crate) fn context_service(&self, arena_id: ArenaId) -> Option<&ContextService<G>> {
        if self.context_service.context.arena_id == arena_id {
            Some(&self.context_service)
        } else {
            self.private_rooms
                .get(&arena_id)
                .map(|room| &room.context_service)
        }
    }

    /// new returns a game server with the specified parameters.
    pub async fn new(
        server_id: Option<ServerId>,
        system: Option<SystemRepo<G>>,
        discord_bot: Option<DiscordBotRepo>,
        discord_oauth2: Option<&'static DiscordOauth2Repo>,
        client_hash: u64,
        region_id: Option<RegionId>,
        database_read_only: bool,
        min_bots: Option<usize>,
        max_bots: Option<usize>,
        bot_percent: Option<usize>,
        chat_log: Option<String>,
        trace_log: Option<String>,
        game_client: Arc<RwLock<MiniCdn>>,
        allow_web_socket_json: &'static AtomicBool,
        admin_config_file: Option<String>,
        client_authenticate: RateLimiterProps,
    ) -> Self {
        // TODO: If multiple arenas, generate randomly.
        let arena_id = ArenaId(
            NonZeroU32::new(server_id.map(|s| s.0.get()).unwrap_or(0) as u32 + 2000).unwrap(),
        );

        Self {
            server_id,
            region_id,
            /// Leak the boxes, because static lifetime facilitates async code. This will probably
            /// only ever happen once, and it will last for the lifetime of the program.
            database: Box::leak(Box::new(Database::new(database_read_only).await)),
            system,
            discord_bot: discord_bot.map(|b| &*Box::leak(Box::new(b))),
            discord_oauth2,
            admin: AdminRepo::new(game_client, admin_config_file, allow_web_socket_json),
            context_service: ContextService::new(
                arena_id,
                false,
                min_bots,
                max_bots,
                bot_percent,
                chat_log,
                trace_log,
                client_authenticate,
            ),
            private_rooms: HashMap::new(),
            next_private_arena_id: 0x8000_0000,
            invitations: InvitationRepo::new(),
            leaderboard: LeaderboardRepo::new(),
            metrics: MetricRepo::new(),
            status: StatusRepo::new(client_hash),
            last_update: Instant::now(),
        }
    }

    /// Call once every tick.
    pub fn update(&mut self, ctx: &mut <Infrastructure<G> as Actor>::Context) {
        let now = Instant::now();
        if now.duration_since(self.last_update) < Duration::from_secs_f32(G::TICK_PERIOD_SECS * 0.5)
        {
            // Less than half a tick elapsed. Drop this update on the floor, to avoid jerking.
            return;
        }
        self.last_update = now;

        let status = &self.status;
        let server_delta = self.system.as_mut().and_then(|system| system.delta(status));
        self.context_service.update(
            &mut self.leaderboard,
            &mut self.invitations,
            &mut self.metrics,
            self.server_id,
            server_delta.clone(),
        );
        for room in self.private_rooms.values_mut() {
            room.context_service.update(
                &mut self.leaderboard,
                &mut self.invitations,
                &mut self.metrics,
                self.server_id,
                server_delta.clone(),
            );
            if room.context_service.context.players.real_players != 0 {
                room.empty_since = Instant::now();
            }
        }
        let expired_rooms: Vec<_> = self
            .private_rooms
            .iter()
            .filter_map(|(&arena_id, room)| {
                (room.context_service.context.players.real_players == 0
                    && Instant::now().duration_since(room.empty_since) >= PRIVATE_ROOM_IDLE_TIMEOUT)
                    .then_some(arena_id)
            })
            .collect();
        for arena_id in expired_rooms {
            self.private_rooms.remove(&arena_id);
            self.invitations.remove_arena(arena_id);
        }
        self.leaderboard.clear_deltas();
        self.status.health.record_tick(G::TICK_PERIOD_SECS);

        // These are all rate-limited internally.
        LeaderboardRepo::update_to_database(self, ctx);
        LeaderboardRepo::update_from_database(self, ctx);
        MetricRepo::update_to_database(self, ctx);
        ClientRepo::update_to_database(self, ctx);
        SystemRepo::update(self, ctx);
    }

    /// Returns a static reference to the database singleton.
    pub fn database(&self) -> &'static Database {
        self.database
    }
}

impl<G: GameArenaService> Handler<CreatePrivateRoom> for Infrastructure<G> {
    type Result = Result<PrivateRoomCreated, &'static str>;

    fn handle(&mut self, request: CreatePrivateRoom, _: &mut Self::Context) -> Self::Result {
        if request.bot_count > MAX_PRIVATE_ROOM_BOTS {
            return Err("bot count exceeds the private room limit");
        }
        if self.private_rooms.len() >= MAX_PRIVATE_ROOMS {
            return Err("too many private rooms are currently active");
        }

        let arena_id = ArenaId(NonZeroU32::new(self.next_private_arena_id).unwrap());
        self.next_private_arena_id = self.next_private_arena_id.wrapping_add(1).max(0x8000_0000);

        let mut context_service = ContextService::new(
            arena_id,
            true,
            Some(request.bot_count),
            Some(request.bot_count),
            Some(100),
            None,
            None,
            RateLimiterProps::new(Duration::from_secs(30), 16),
        );
        context_service.set_fixed_bot_count(request.bot_count);

        let secret = loop {
            let secret = rand::random::<u64>();
            if secret != 0
                && self
                    .private_rooms
                    .values()
                    .all(|room| room.secret != secret)
            {
                break secret;
            }
        };
        let (invitation_id, code) = self
            .invitations
            .create_private_invitation(arena_id, self.server_id);
        self.private_rooms.insert(
            arena_id,
            PrivateRoom {
                context_service,
                secret,
                empty_since: Instant::now(),
            },
        );

        Ok(PrivateRoomCreated {
            invitation_id,
            code,
            secret,
        })
    }
}

impl<G: GameArenaService> Handler<SetPrivateRoomBotCount> for Infrastructure<G> {
    type Result = Result<(), &'static str>;

    fn handle(&mut self, request: SetPrivateRoomBotCount, _: &mut Self::Context) -> Self::Result {
        if request.bot_count > MAX_PRIVATE_ROOM_BOTS {
            return Err("bot count exceeds the private room limit");
        }
        let invitation = self
            .invitations
            .get(request.invitation_id)
            .ok_or("private room is closed")?;
        let room = self
            .private_rooms
            .get_mut(&invitation.arena_id)
            .ok_or("private room is closed")?;
        if room.secret != request.secret {
            return Err("invalid private room configuration token");
        }
        room.context_service.set_fixed_bot_count(request.bot_count);
        Ok(())
    }
}
