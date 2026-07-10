use spacetimedb::{Identity, ReducerContext, Table, Timestamp};

#[spacetimedb::table(accessor = user, public)]
pub struct User {
    #[primary_key]
    identity: Identity,
    name: Option<String>,
    online: bool,
    x: f32,
    y: f32,
    last_seen: Timestamp,
}

#[spacetimedb::reducer]
pub fn set_pos(ctx: &ReducerContext, x: f32, y: f32) -> Result<(), String> {
    if let Some(user) = ctx.db.user().identity().find(ctx.sender()) {
        ctx.db.user().identity().update(User {
            x,
            y,
            last_seen: ctx.timestamp,
            ..user
        });
        Ok(())
    } else {
        Err("Cannot set pos for unknown user".to_string())
    }
}

#[spacetimedb::reducer]
pub fn set_name(ctx: &ReducerContext, name: String) -> Result<(), String> {
    if name.is_empty() {
        return Err("Names must not be empty".to_string());
    }
    if let Some(user) = ctx.db.user().identity().find(ctx.sender()) {
        ctx.db.user().identity().update(User { name: Some(name), ..user });
        Ok(())
    } else {
        Err("Cannot set name for unknown user".to_string())
    }
}

#[spacetimedb::reducer(client_connected)]
pub fn client_connected(ctx: &ReducerContext) {
    // No client IP here — the module only ever sees identity/connection_id;
    // the real IP is visible one layer up, in Caddy's access log for
    // spacetime.mister-esman.uk.
    log::info!(
        "client connected: identity={:?} connection={:?}",
        ctx.sender(),
        ctx.connection_id()
    );
    if let Some(user) = ctx.db.user().identity().find(ctx.sender()) {
        ctx.db.user().identity().update(User { online: true, ..user });
    } else {
        ctx.db.user().insert(User {
            identity: ctx.sender(),
            name: None,
            online: true,
            x: 360.0,
            y: 360.0,
            last_seen: ctx.timestamp,
        });
    }
}

#[spacetimedb::reducer(client_disconnected)]
pub fn identity_disconnected(ctx: &ReducerContext) {
    log::info!(
        "client disconnected: identity={:?} connection={:?}",
        ctx.sender(),
        ctx.connection_id()
    );
    if let Some(user) = ctx.db.user().identity().find(ctx.sender()) {
        ctx.db.user().identity().update(User { online: false, ..user });
    } else {
        log::warn!("Disconnect event for unknown user {:?}", ctx.sender());
    }
}
