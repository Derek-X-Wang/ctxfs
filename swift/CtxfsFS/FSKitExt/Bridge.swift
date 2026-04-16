import FSKit
import Foundation
import os

final class Bridge: FSUnaryFileSystem, FSUnaryFileSystemOperations {

    static let shared = Bridge()

    private let log = Logger(subsystem: "FSKitExt", category: "Bridge")

    private let socket = Socket.shared

    private override init() {
        super.init()
    }

    func probeResource(
        resource: FSResource,
        replyHandler: @escaping (FSProbeResult?, (any Error)?) -> Void
    ) {
        // Return usable unconditionally — the ctxfs daemon is the one that
        // initiates the mount via mounter::mount(), so probe always succeeds.
        // The socket isn't configured until loadResource (where we receive the
        // auth token via FSTaskOptions), so we can't query the daemon here.
        log.d("probeResource: returning usable (auth token deferred to loadResource)")
        replyHandler(
            FSProbeResult.usable(
                name: "ctxfs",
                containerID: FSContainerIdentifier(uuid: UUID())
            ),
            nil
        )
    }

    func loadResource(
        resource: FSResource,
        options: FSTaskOptions,
        replyHandler: @escaping (FSVolume?, (any Error)?) -> Void
    ) {
        log.d("loadResource")
        do {
            let port = try Bundle.main.getServerPort()
            var token: Data?
            for opt in options.taskOptions {
                if opt.hasPrefix("token=") {
                    let hex = String(opt.dropFirst("token=".count))
                    token = Data(hexString: hex)
                }
            }
            guard let token else {
                log.e("loadResource: missing auth token in task options")
                replyHandler(nil, nil)
                return
            }
            socket.initialize(host: "localhost", port: port, token: token)

            let response = try socket.send(
                content: .getVolumeIdentifier(Pb_GetVolumeIdentifier())
            )
            if case .volumeIdentifier(let value) = response {
                let volume = Volume(value)
                volume.load()
                containerStatus = .ready
                replyHandler(volume, nil)
                return
            }
        } catch {
            log.e(
                "loadResource: failure (error = \(error.localizedDescription))"
            )
        }
        replyHandler(nil, nil)
    }

    func unloadResource(
        resource: FSResource,
        options: FSTaskOptions,
        replyHandler reply: @escaping ((any Error)?) -> Void
    ) {
        log.d("unloadResource")
        reply(nil)
    }

    func didFinishLoading() {
        log.d("didFinishLoading")
    }
}
