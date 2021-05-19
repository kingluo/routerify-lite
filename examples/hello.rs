use hyper::{header, Body, Request, Response, Server, StatusCode};
use routerify_lite::{RequestExt, Router, RouterService};
use std::{convert::Infallible, net::SocketAddr};

async fn any_method_handler(_req: Request<Body>) -> Result<Response<Body>, Infallible> {
    Ok(Response::new(Body::from("any method")))
}

async fn hello_handler(req: Request<Body>) -> Result<Response<Body>, Infallible> {
    let user = req.param("name").unwrap();
    Ok(Response::new(Body::from(format!("Hello {}", user))))
}

async fn err_404_handler(_: Request<Body>) -> Result<Response<Body>, Infallible> {
    Ok(Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(hyper::Body::from(
            StatusCode::NOT_FOUND.canonical_reason().unwrap(),
        ))
        .expect("Couldn't create the default 404 response"))
}

fn router() -> Router<Body, Infallible> {
    Router::new()
        //.set_plain()
        .get("/", |_| async {
            Ok(Response::new(Body::from("Home page")))
        })
        .get("/hello/:name", hello_handler)
        .any_method("/any_method", any_method_handler)
        .any(err_404_handler)
        .build()
        .unwrap()
}

#[tokio::main]
async fn main() {
    // Create a Service from the router above to handle incoming requests.
    let router = router();

    let service = RouterService::new(router);

    // The address on which the server will be listening.
    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));

    // Create a server by passing the created service to `.serve` method.
    let server = Server::bind(&addr).serve(service);

    println!("App is running on: {}", addr);
    if let Err(err) = server.await {
        eprintln!("Server error: {}", err);
    }
}
