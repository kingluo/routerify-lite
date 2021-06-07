use futures::Future;
use futures_util::future;
use hyper::{body::HttpBody, server::conn::AddrStream, service::Service, HeaderMap, Uri, Version};
use hyper::{header, Body, Method, Request, Response, StatusCode};
use lazy_static::lazy_static;
use percent_encoding::percent_decode_str;
use regex::Regex;
use regex::RegexSet;
use std::any::Any;
use std::pin::Pin;
use std::{collections::HashMap, fmt::Debug};
use std::{convert::Infallible, error::Error as StdError};
use std::{
    sync::Arc,
    task::{Context, Poll},
};

pub type RouteError = Box<dyn StdError + Send + Sync + 'static>;

type Handler<B, E> =
    Box<dyn Fn(Request<hyper::Body>) -> HandlerReturn<B, E> + Send + Sync + 'static>;
type HandlerReturn<B, E> = Box<dyn Future<Output = Result<Response<B>, E>> + Send + 'static>;

type ErrHandlerWithoutInfo<B> =
    Box<dyn Fn(RouteError) -> ErrHandlerWithoutInfoReturn<B> + Send + Sync + 'static>;
type ErrHandlerWithoutInfoReturn<B> = Box<dyn Future<Output = Response<B>> + Send + 'static>;

type ErrHandlerWithInfo<B> =
    Box<dyn Fn(RouteError, RequestInfo) -> ErrHandlerWithInfoReturn<B> + Send + Sync + 'static>;
type ErrHandlerWithInfoReturn<B> = Box<dyn Future<Output = Response<B>> + Send + 'static>;

fn percent_decode_request_path(val: &str) -> std::result::Result<String, RouteError> {
    percent_decode_str(val)
        .decode_utf8()
        .map_err(|e| e.into())
        .map(|val| val.to_string())
}

pub struct RequestService<B, E> {
    router: Arc<Router<B, E>>,
}

impl<
        B: HttpBody + Send + Sync + 'static,
        E: Into<Box<dyn std::error::Error + Send + Sync>> + 'static,
    > Service<Request<Body>> for RequestService<B, E>
{
    type Response = Response<B>;
    type Error = crate::RouteError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        let path = percent_decode_request_path(req.uri().path());
        if let Err(err) = path {
            return Box::pin(async { Err(err) });
        }

        let mut path = path.unwrap();

        if path.is_empty() {
            path.push('/');
        }

        let router = self.router.clone();

        let req_info = match router.err_handler {
            Some(ErrHandler::WithInfo(_)) => Some(RequestInfo::new_from_req(&req)),
            _ => None,
        };

        if !router.plain.is_empty() {
            let mut k = PathMethod {
                path: path.clone(),
                method: Some(req.method().clone()),
            };
            let mut i = router.plain.get(&k);
            if i.is_none() {
                k.method = None;
                i = router.plain.get(&k);
            }
            if !i.is_none() {
                let i = *i.unwrap();
                let fut = async move {
                    let route_resp_res = Pin::from(router.handlers2[i](req))
                        .await
                        .map_err(Into::into);
                    let route_resp = match route_resp_res {
                        Ok(route_resp) => route_resp,
                        Err(err) => {
                            if let Some(ref err_handler) = router.err_handler {
                                err_handler.execute(err, req_info).await
                            } else {
                                unreachable!(err);
                            }
                        }
                    };

                    Ok(route_resp)
                };
                return Box::pin(fut);
            }
        }

        for i in router.regex_set.matches(&path).into_iter() {
            let meta = &router.meta[i];
            if !meta.is_method_match(req.method()) {
                continue;
            }

            if meta.regex.is_some() {
                let mut hash = HashMap::with_capacity(meta.params.len());

                if let Some(caps) = meta.regex.as_ref().unwrap().captures(&path) {
                    let mut iter = caps.iter();
                    // Skip the first match because it's the whole path.
                    iter.next();
                    for param in meta.params.iter() {
                        if let Some(Some(g)) = iter.next() {
                            hash.insert(param.clone(), g.as_str().to_string());
                        }
                    }
                }

                req.extensions_mut().insert(RequestMeta {
                    route_params: Some(RouteParams(hash)),
                });
            }

            let fut = async move {
                let route_resp_res = Pin::from(router.handlers[i](req)).await.map_err(Into::into);
                let route_resp = match route_resp_res {
                    Ok(route_resp) => route_resp,
                    Err(err) => {
                        if let Some(ref err_handler) = router.err_handler {
                            err_handler.execute(err, req_info).await
                        } else {
                            unreachable!(err);
                        }
                    }
                };

                Ok(route_resp)
            };

            return Box::pin(fut);
        }

        unreachable!("no request handler matched");
    }
}

pub struct RouterService<B, E> {
    router: Arc<Router<B, E>>,
}

impl<
        B: HttpBody + Send + Sync + 'static,
        E: Into<Box<dyn std::error::Error + Send + Sync>> + 'static,
    > RouterService<B, E>
{
    pub fn new(router: Router<B, E>) -> RouterService<B, E> {
        RouterService {
            router: Arc::from(router),
        }
    }
}

impl<
        B: HttpBody + Send + Sync + 'static,
        E: Into<Box<dyn std::error::Error + Send + Sync>> + 'static,
    > Service<&AddrStream> for RouterService<B, E>
{
    type Response = RequestService<B, E>;
    type Error = Infallible;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: &AddrStream) -> Self::Future {
        future::ok(RequestService {
            router: self.router.clone(),
        })
    }
}

pub struct RouteParams(HashMap<String, String>);

impl RouteParams {
    fn get<N: Into<String>>(&self, param_name: N) -> Option<&String> {
        self.0.get(&param_name.into())
    }
}

pub trait RequestExt {
    fn params(&self) -> &RouteParams;
    fn param<P: Into<String>>(&self, param_name: P) -> Option<&String>;
}

struct RequestMeta {
    route_params: Option<RouteParams>,
}

impl RequestMeta {
    fn route_params(&self) -> Option<&RouteParams> {
        self.route_params.as_ref()
    }
}

impl RequestExt for Request<hyper::Body> {
    fn params(&self) -> &RouteParams {
        self.extensions()
            .get::<RequestMeta>()
            .and_then(|meta| meta.route_params())
            .expect("Routerify: No RouteParams added while processing request")
    }

    fn param<P: Into<String>>(&self, param_name: P) -> Option<&String> {
        self.params().get(&param_name.into())
    }
}

#[derive(Debug)]
struct PathMeta {
    method: Option<Method>,
    regex: Option<Regex>,
    params: Vec<String>,
}

impl PathMeta {
    fn new() -> PathMeta {
        PathMeta {
            method: None,
            regex: None,
            params: vec![],
        }
    }

    fn is_method_match(&self, method: &Method) -> bool {
        self.method.is_none() || self.method.as_ref() == Some(method)
    }
}

enum ErrHandler<B> {
    WithoutInfo(ErrHandlerWithoutInfo<B>),
    WithInfo(ErrHandlerWithInfo<B>),
}

impl<B: HttpBody + Send + Sync + 'static> ErrHandler<B> {
    async fn execute(&self, err: RouteError, req_info: Option<RequestInfo>) -> Response<B> {
        match self {
            ErrHandler::WithoutInfo(ref err_handler) => Pin::from(err_handler(err)).await,
            ErrHandler::WithInfo(ref err_handler) => {
                Pin::from(err_handler(
                    err,
                    req_info.expect("No RequestInfo is provided"),
                ))
                .await
            }
        }
    }
}

#[derive(Debug)]
pub struct RequestInfo {
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    version: Version,
}

impl RequestInfo {
    pub fn new_from_req(req: &Request<Body>) -> Self {
        RequestInfo {
            headers: req.headers().clone(),
            method: req.method().clone(),
            uri: req.uri().clone(),
            version: req.version(),
        }
    }

    /// Returns the request headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Returns the request method type.
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// Returns the request uri.
    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// Returns the request's HTTP version.
    pub fn version(&self) -> Version {
        self.version
    }
}

#[derive(Hash, PartialEq, Eq, Debug)]
struct PathMethod {
    path: String,
    method: Option<Method>,
}

lazy_static! {
    static ref PATH_PARAMS_RE: Regex = Regex::new(r"(?s)(?::([^/]+))|(?:\*)").unwrap();
}

fn generate_common_regex_str(path: &str) -> (String, Vec<String>) {
    let mut regex_str = String::with_capacity(path.len());
    let mut param_names = Vec::new();

    let mut pos: usize = 0;

    for caps in PATH_PARAMS_RE.captures_iter(path) {
        let whole = caps.get(0).unwrap();

        let path_s = &path[pos..whole.start()];
        regex_str += &regex::escape(path_s);

        if whole.as_str() == "*" {
            regex_str += r"(.*)";
            param_names.push("*".to_owned());
        } else {
            regex_str += r"([^/]+)";
            param_names.push(caps.get(1).unwrap().as_str().to_owned());
        }

        pos = whole.end();
    }

    let left_over_path_s = &path[pos..];
    regex_str += &regex::escape(left_over_path_s);

    (regex_str, param_names)
}

pub struct Router<B, E> {
    meta: Vec<PathMeta>,
    meta2: Vec<PathMeta>,
    routes: Vec<String>,
    routes2: Vec<String>,
    handlers: Vec<Handler<B, E>>,
    handlers2: Vec<Handler<B, E>>,
    err_handler: Option<ErrHandler<B>>,
    regex_set: RegexSet,
    plain: HashMap<PathMethod, usize>,
}

impl<
        B: HttpBody + Send + Sync + 'static,
        E: Into<Box<dyn std::error::Error + Send + Sync>> + 'static,
    > Router<B, E>
{
    pub fn new() -> Router<B, E> {
        Router {
            meta: vec![],
            routes: vec![],
            handlers: vec![],
            meta2: vec![],
            routes2: vec![],
            handlers2: vec![],
            err_handler: None,
            regex_set: RegexSet::empty(),
            plain: HashMap::new(),
        }
    }

    fn downcast_to_hyper_body_type(&mut self) -> Option<&mut Router<hyper::Body, E>> {
        let any_obj: &mut dyn Any = self;
        any_obj.downcast_mut::<Router<hyper::Body, E>>()
    }

    fn init_default_404_route(&mut self) {
        let found = self
            .routes
            .iter()
            .enumerate()
            .any(|(i, path)| path == "(?s)^/(.*)$" && self.meta[i].method.is_none());

        if found {
            return;
        }

        if let Some(router) = self.downcast_to_hyper_body_type() {
            let mut meta = PathMeta::new();
            meta.method = None;

            router.meta.push(meta);

            router.routes.push("(?s)^/(.*)$".to_string());

            let handler = |_req| async move {
                Ok(Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(hyper::Body::from(
                        StatusCode::NOT_FOUND.canonical_reason().unwrap(),
                    ))
                    .expect("Couldn't create the default 404 response"))
            };
            router
                .handlers
                .push(Box::new(move |req: Request<hyper::Body>| {
                    Box::new(handler(req))
                }));
        }
    }

    fn init_err_handler(&mut self) {
        let found = self.err_handler.is_some();

        if found {
            return;
        }

        if let Some(router) = self.downcast_to_hyper_body_type() {
            let handler: ErrHandler<hyper::Body> =
                ErrHandler::WithoutInfo(Box::new(move |err: RouteError| {
                    Box::new(async move {
                        Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .header(header::CONTENT_TYPE, "text/plain")
                            .body(hyper::Body::from(format!(
                                "{}: {}",
                                StatusCode::INTERNAL_SERVER_ERROR
                                    .canonical_reason()
                                    .unwrap(),
                                err
                            )))
                            .expect("Couldn't create a response while handling the server error")
                    })
                }));
            router.err_handler = Some(handler);
        } else {
            eprintln!(
                "Warning: No error handler added. It is recommended to add one to see what went wrong if any route or middleware fails.\n\
                Please add one by calling `.err_handler(handler)` method of the root router builder.\n"
            );
        }
    }

    pub fn err_handler<H, R>(mut self, handler: H) -> Self
    where
        H: Fn(RouteError) -> R + Send + Sync + 'static,
        R: Future<Output = Response<B>> + Send + 'static,
    {
        let handler: ErrHandlerWithoutInfo<B> =
            Box::new(move |err: RouteError| Box::new(handler(err)));
        self.err_handler = Some(ErrHandler::WithoutInfo(handler));
        self
    }

    pub fn err_handler_with_info<H, R>(mut self, handler: H) -> Self
    where
        H: Fn(RouteError, RequestInfo) -> R + Send + Sync + 'static,
        R: Future<Output = Response<B>> + Send + 'static,
    {
        let handler: ErrHandlerWithInfo<B> =
            Box::new(move |err: RouteError, req_info: RequestInfo| {
                Box::new(handler(err, req_info))
            });
        self.err_handler = Some(ErrHandler::WithInfo(handler));
        self
    }

    fn add<P, H, R>(mut self, path: P, method: Option<Method>, handler: H) -> Self
    where
        P: Into<String>,
        H: Fn(Request<hyper::Body>) -> R + Send + Sync + 'static,
        R: Future<Output = Result<Response<B>, E>> + Send + 'static,
    {
        let path = path.into();

        let mut meta = PathMeta::new();
        meta.method = method;

        let (path, params) = generate_common_regex_str(&path);
        if !params.is_empty() {
            let re_str = format!("{}{}{}", r"(?s)^", &path, "$");
            meta.regex = Some(Regex::new(re_str.as_str()).unwrap());
            meta.params = params;
            self.meta.push(meta);

            self.routes.push(re_str);

            self.handlers
                .push(Box::new(move |req: Request<hyper::Body>| {
                    Box::new(handler(req))
                }));
        } else {
            self.meta2.push(meta);

            self.routes2.push(path);

            self.handlers2
                .push(Box::new(move |req: Request<hyper::Body>| {
                    Box::new(handler(req))
                }));
        }

        self
    }

    pub fn get<P, H, R>(self, path: P, handler: H) -> Self
    where
        P: Into<String>,
        H: Fn(Request<hyper::Body>) -> R + Send + Sync + 'static,
        R: Future<Output = Result<Response<B>, E>> + Send + 'static,
    {
        self.add(path, Some(Method::GET), handler)
    }

    pub fn post<P, H, R>(self, path: P, handler: H) -> Self
    where
        P: Into<String>,
        H: Fn(Request<hyper::Body>) -> R + Send + Sync + 'static,
        R: Future<Output = Result<Response<B>, E>> + Send + 'static,
    {
        self.add(path, Some(Method::POST), handler)
    }

    pub fn put<P, H, R>(self, path: P, handler: H) -> Self
    where
        P: Into<String>,
        H: Fn(Request<hyper::Body>) -> R + Send + Sync + 'static,
        R: Future<Output = Result<Response<B>, E>> + Send + 'static,
    {
        self.add(path, Some(Method::PUT), handler)
    }

    pub fn delete<P, H, R>(self, path: P, handler: H) -> Self
    where
        P: Into<String>,
        H: Fn(Request<hyper::Body>) -> R + Send + Sync + 'static,
        R: Future<Output = Result<Response<B>, E>> + Send + 'static,
    {
        self.add(path, Some(Method::DELETE), handler)
    }

    pub fn head<P, H, R>(self, path: P, handler: H) -> Self
    where
        P: Into<String>,
        H: Fn(Request<hyper::Body>) -> R + Send + Sync + 'static,
        R: Future<Output = Result<Response<B>, E>> + Send + 'static,
    {
        self.add(path, Some(Method::HEAD), handler)
    }

    pub fn trace<P, H, R>(self, path: P, handler: H) -> Self
    where
        P: Into<String>,
        H: Fn(Request<hyper::Body>) -> R + Send + Sync + 'static,
        R: Future<Output = Result<Response<B>, E>> + Send + 'static,
    {
        self.add(path, Some(Method::TRACE), handler)
    }

    pub fn connect<P, H, R>(self, path: P, handler: H) -> Self
    where
        P: Into<String>,
        H: Fn(Request<hyper::Body>) -> R + Send + Sync + 'static,
        R: Future<Output = Result<Response<B>, E>> + Send + 'static,
    {
        self.add(path, Some(Method::CONNECT), handler)
    }

    pub fn patch<P, H, R>(self, path: P, handler: H) -> Self
    where
        P: Into<String>,
        H: Fn(Request<hyper::Body>) -> R + Send + Sync + 'static,
        R: Future<Output = Result<Response<B>, E>> + Send + 'static,
    {
        self.add(path, Some(Method::PATCH), handler)
    }

    pub fn options<P, H, R>(self, path: P, handler: H) -> Self
    where
        P: Into<String>,
        H: Fn(Request<hyper::Body>) -> R + Send + Sync + 'static,
        R: Future<Output = Result<Response<B>, E>> + Send + 'static,
    {
        self.add(path, Some(Method::OPTIONS), handler)
    }

    pub fn any<H, R>(self, handler: H) -> Self
    where
        H: Fn(Request<hyper::Body>) -> R + Send + Sync + 'static,
        R: Future<Output = Result<Response<B>, E>> + Send + 'static,
    {
        self.add("/*", None, handler)
    }

    pub fn any_method<H, R, P>(self, path: P, handler: H) -> Self
    where
        P: Into<String>,
        H: Fn(Request<hyper::Body>) -> R + Send + Sync + 'static,
        R: Future<Output = Result<Response<B>, E>> + Send + 'static,
    {
        self.add(path, None, handler)
    }

    pub fn build(mut self) -> Result<Self, RouteError> {
        self.init_default_404_route();

        self.init_err_handler();

        for (i, r) in self.routes2.iter().enumerate() {
            let path_method = PathMethod {
                path: r.clone(),
                method: self.meta2[i].method.clone(),
            };
            if self.plain.contains_key(&path_method) {
                continue;
            }
            self.plain.insert(path_method, i);
        }

        if !self.routes.is_empty() {
            self.regex_set = RegexSet::new(&self.routes)?;
        }

        Ok(self)
    }
}
