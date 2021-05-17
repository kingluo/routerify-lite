use hyper::{Body, Request, Response, Server, StatusCode};
use routerify_lite::{RouteError, Router, RouterService};
use std::net::SocketAddr;

async fn hello_handler(_req: Request<Body>) -> Result<Response<Body>, RouteError> {
    Err(Box::new(std::io::Error::new(
        std::io::ErrorKind::Other,
        "Some errors",
    )))
}

// The error handler will accept the thrown error in routerify::Error type and
// it will have to generate a response based on the error.
async fn err_handler(err: RouteError) -> Response<Body> {
    eprintln!("error: {:?}", err);
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(Body::from("Something went wrong"))
        .unwrap()
}

// async fn err_handler_with_info(err: RouteError, req: RequestInfo) -> Response<Body> {
//     eprintln!("req: {:?}, error: {:?}", req, err);
//     Response::builder()
//         .status(StatusCode::INTERNAL_SERVER_ERROR)
//         .body(Body::from("Something went wrong"))
//         .unwrap()
// }

fn router() -> Router<Body, RouteError> {
    Router::new()
        .get("/", |_| async {
            Ok(Response::new(Body::from("Home page")))
        })
        .get("/hello/:name", hello_handler)
        .err_handler(err_handler)
        // .err_handler_with_info(err_handler_with_info)
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
