## File Sharing
## An innovative high-performance file sharing service, which allows for direct connection built in pure rust!
### Great performance with over 2GBPS peak bandwidth
### Features :
  - Does NOT use any external service, the server and the client are all in the source code.
  - IPv6 & IPv4 (Experimental) support
  - Blazing fast downloads and uploads due to the QUIC protocol usage (Will be improved in the future for EVEN better speeds)
  - Ability to share folders, so users can download every file from the folders shared
  - Interactive Ui
  - Can easily be made secure with Ceritificates and encryption
### Techstack:
  - Ui: egui & egui-notify
  - Backend: tokio-rs
  - Diagnostic information: tracing
  - Networking: quinn (A QUIC implmentation)
