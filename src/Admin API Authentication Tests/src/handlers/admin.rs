use actix_web::{web, HttpRequest, HttpResponse, Responder};
use serde_json::json;

use crate::models::Claims;

pub async fn admin_dashboard(req: HttpRequest) -> impl Responder {
    let claims = req.extensions().get::<Claims>().cloned();
    
    match claims {
        Some(claims) => HttpResponse::Ok().json(json!({
            "message": "Admin dashboard",
            "user": claims.sub,
            "role": claims.role
        })),
        None => HttpResponse::Unauthorized().json(json!({
            "error": "Unauthorized"
        })),
    }
}

pub async fn admin_users(req: HttpRequest) -> impl Responder {
    let claims = req.extensions().get::<Claims>().cloned();
    
    match claims {
        Some(claims) => HttpResponse::Ok().json(json!({
            "message": "User management",
            "admin": claims.sub,
            "users": ["user1", "user2", "user3"]
        })),
        None => HttpResponse::Unauthorized().json(json!({
            "error": "Unauthorized"
        })),
    }
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/admin")
            .route("/dashboard", web::get().to(admin_dashboard))
            .route("/users", web::get().to(admin_users))
    );
}
