import XCTest
@testable import Paku

final class ServiceConfigurationTests: XCTestCase {
    func testHostedDefaultsAreExplicitlyUnconfigured() {
        XCTAssertNil(Endpoints.edgeURL)
        XCTAssertNil(Endpoints.workosClientId)
        XCTAssertNil(Endpoints.authorizeURL(state: "nonce"))
        XCTAssertEqual(AppModel.edgeURL, "")
    }

    func testIncompleteConfigurationCannotStartWorkOS() {
        let edge = URL(string: "https://service.example.test")!
        XCTAssertNil(Endpoints.authorizeURL(state: "nonce", edgeURL: nil, clientId: "own-client"))
        XCTAssertNil(Endpoints.authorizeURL(state: "nonce", edgeURL: edge, clientId: nil))
        XCTAssertNil(Endpoints.authorizeURL(state: "nonce", edgeURL: edge, clientId: "  "))
        XCTAssertNil(Endpoints.authorizeURL(state: "nonce", edgeURL: edge, clientId: "$(UNSET_CLIENT_ID)"))
    }

    func testOwnClientConfigurationAndStateAreUsed() throws {
        let url = try XCTUnwrap(Endpoints.authorizeURL(state: "nonce & +", edgeURL: URL(string: "https://service.example.test"), clientId: "own-client"))
        let items = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems)
        XCTAssertEqual(items.first { $0.name == "client_id" }?.value, "own-client")
        XCTAssertEqual(items.first { $0.name == "state" }?.value, "nonce & +")
        XCTAssertEqual(items.first { $0.name == "redirect_uri" }?.value, "\(Endpoints.callbackScheme)://callback")
    }

    func testDevEdgeMustBeExplicitAndValid() {
        XCTAssertNil(Endpoints.serviceURL(nil))
        XCTAssertNil(Endpoints.serviceURL(""))
        XCTAssertNil(Endpoints.serviceURL("$(UNSET_EDGE)"))
        XCTAssertNil(Endpoints.serviceURL("not a URL"))
        XCTAssertNil(Endpoints.serviceURL("file:///tmp/service"))
        XCTAssertNil(Endpoints.serviceURL("http://service.example.test"))
        XCTAssertNil(Endpoints.serviceURL("https://user:password@service.example.test"))
        XCTAssertNil(Endpoints.serviceURL("https://service.example.test?token=secret"))
        XCTAssertEqual(Endpoints.serviceURL("http://localhost:27650")?.port, 27650)
        XCTAssertNotNil(Endpoints.serviceURL("http://127.0.0.1:27650"))
        XCTAssertNotNil(Endpoints.serviceURL("https://service.example.test"))
    }
}
