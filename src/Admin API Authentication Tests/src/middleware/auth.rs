use actix_web::{
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    Error, HttpMessage, HttpResponse,
};
use futures::future::LocalBoxFuture;
use jsonwebtoken::{decode, DecodingKey, Validation};
use std::future::{ready, Ready};

use crate::models::{Claims, Role};

const JWT_SECRET: &[u8] = b"test_secret_key";

pub struct AuthMiddleware {
    pub required_role: Option<Role>,
}

impl AuthMiddleware {
    pub fn new(required_role: Option<Role>) -> Self {
        Self { required_role }
    }
}

impl<S, B> Transform<S, ServiceRequest> for AuthMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type InitError = ();
    type Transform = AuthMiddlewareService<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(AuthMiddlewareService {
            service,
            required_role: self.required_role.clone(),
        }))
    }
}

pub struct AuthMiddlewareService<S> {
    service: S,
    required_role: Option<Role>,
}

impl<S, B> Service<ServiceRequest> for AuthMiddlewareService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let required_role = self.required_role.clone();
        
        let auth_header = req.headers().get("Authorization");
        
        if auth_header.is_none() {
            return Box::pin(async move {
                Err(actix_web::error::ErrorUnauthorized("Missing credentials"))
            });
        }

        let auth_str = match auth_header.unwrap().to_str() {
            Ok(s) => s,
            Err(_) => {
                return Box::pin(async move {
                    Err(actix_web::error::ErrorUnauthorized("Invalid credentials"))
                });
            }
        };

        let token = if auth_str.starts_with("Bearer ") {
            &auth_str[7..]
        } else {
            return Box::pin(async move {
                Err(actix_web::error::ErrorUnauthorized("Invalid credentials"))
            });
        };

        let validation = Validation::default();
        let token_data = match decode::<Claims>(
            token,
            &DecodingKey::from_secret(JWT_SECRET),
            &validation,
        ) {
            Ok(data) => data,
            Err(e) => {
                return Box::pin(async move {
                    let error_msg = match e.kind() {
                        jsonwebtoken::errors::ErrorKind::ExpiredSignature => "Expired token",
                        _ => "Invalid credentials",
                    };
                    Err(actix_web::error::ErrorUnauthorized(error_msg))
                });
            }
        };

        if let Some(required) = required_role {
            if token_data.claims.role != required {
                return Box::pin(async move {
                    Err(actix_web::error::ErrorForbidden("Insufficient permissions"))
                });
            }
        }

        req.extensions_mut().insert(token_data.claims);

        let fut = self.service.call(req);
        Box::pin(async move {
            let res = fut.await?;
            Ok(res)
        })
    }
}
