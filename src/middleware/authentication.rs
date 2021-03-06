use std::pin::Pin;
use std::task::{Context, Poll};

use actix_service::{Service, Transform};
use actix_web::{dev::ServiceRequest, dev::ServiceResponse, Error};
use futures::future::{err, ok, Ready};
use futures::Future;

use crate::token::authenticate_claim_from_headers;

pub struct Authentication;

impl<S, B> Transform<S> for Authentication
where
    S: Service<Request = ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Request = ServiceRequest;
    type Response = ServiceResponse<B>;
    type Error = Error;
    type InitError = ();
    type Transform = AuthenticationMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(AuthenticationMiddleware { service })
    }
}

pub struct AuthenticationMiddleware<S> {
    service: S,
}

impl<S, B> Service for AuthenticationMiddleware<S>
where
    S: Service<Request = ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Request = ServiceRequest;
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: ServiceRequest) -> Self::Future {
        let claim = authenticate_claim_from_headers(req.headers());
        // TODO: verify revoked tokens

        match claim {
            Ok(application_claim) => {
                let (http_req, payload) = req.into_parts();
                {
                    let mut extensions = http_req.extensions_mut();
                    extensions.insert(application_claim.inner.account_id);
                    extensions.insert(application_claim);
                }
                let new_req =
                    ServiceRequest::from_parts(http_req, payload).unwrap_or_else(|_| panic!("???"));
                Box::pin(self.service.call(new_req))
            }
            Err(error) => Box::pin(err(Error::from(error))),
        }
    }
}
