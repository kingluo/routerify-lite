use futures::Future;
use futures_util::future;
use hyper::{body::HttpBody, server::conn::AddrStream, service::Service, HeaderMap, Uri, Version};
use hyper::{Body, Method, Request, Response};
use percent_encoding::percent_decode_str;
use regex::RegexSet;
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

        if router.is_plain {
            let mut k = PathMethod {
                path: path.clone(),
                method: Some(req.method().clone()),
            };
            let mut i = router.plain.get(&k);
            if i == None {
                k.method = None;
                i = router.plain.get(&k);
            }
            if i == None {
                k.path = "".to_string();
                i = router.plain.get(&k);
            }
            if i == None {
                unreachable!("no request handler matched");
            }
            let i = *i.unwrap();
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

        for i in router.regex_set.matches(&path).into_iter() {
            let meta = &router.meta[i];
            if !meta.is_method_match(req.method()) {
                continue;
            }

            if !meta.sub.is_empty() {
                let r: Vec<&str> = path.split("/").collect();
                let mut hash = HashMap::with_capacity(meta.sub.len());
                for i in &meta.sub {
                    hash.insert(i.1.clone(), r[i.0].to_string());
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
    sub: Vec<(usize, String)>,
}

impl PathMeta {
    fn new() -> PathMeta {
        PathMeta {
            method: None,
            sub: vec![],
        }
    }

    fn is_method_match(&self, method: &Method) -> bool {
        self.method == None || self.method.as_ref() == Some(method)
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

pub struct Router<B, E> {
    meta: Vec<PathMeta>,
    routes: Vec<String>,
    handlers: Vec<Handler<B, E>>,
    err_handler: Option<ErrHandler<B>>,
    regex_set: RegexSet,
    plain: HashMap<PathMethod, usize>,
    is_plain: bool,
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
            err_handler: None,
            regex_set: RegexSet::empty(),
            plain: HashMap::new(),
            is_plain: false,
        }
    }

    pub fn set_plain(mut self) -> Self {
        if !self.routes.is_empty() {
            panic!("set_plain() must be invoked before any route handler get bound");
        }
        self.is_plain = true;
        self
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

        if self.is_plain {
            self.meta.push(meta);
            self.routes.push(path);
        } else {
            let mut res = vec![];
            for (i, str) in path.split("/").enumerate() {
                if str == "" {
                    res.push(str);
                    continue;
                }
                if str.as_bytes()[0] == b':' {
                    res.push("[^/]+");

                    meta.sub.push((i, str.chars().skip(1).collect()));
                } else {
                    res.push(str);
                }
            }
            self.meta.push(meta);

            let res = res.join("/");
            let res = format!("^{}$", res);
            self.routes.push(res);
        }

        self.handlers
            .push(Box::new(move |req: Request<hyper::Body>| {
                Box::new(handler(req))
            }));

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
        if self.is_plain {
            self.add("", None, handler)
        } else {
            self.add("/.*", None, handler)
        }
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
        if self.is_plain {
            return self.build_plain();
        }
        self.regex_set = RegexSet::new(&self.routes)?;
        Ok(self)
    }

    fn build_plain(mut self) -> Result<Self, RouteError> {
        for (i, r) in self.routes.iter().enumerate() {
            let path_method = PathMethod {
                path: r.clone(),
                method: self.meta[i].method.clone(),
            };
            if self.plain.contains_key(&path_method) {
                continue;
            }
            self.plain.insert(path_method, i);
        }

        Ok(self)
    }
}
