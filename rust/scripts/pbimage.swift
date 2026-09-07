// pbimage — write the image on the macOS clipboard to a file.
//
//   pbimage <out.png>
//
// Prints the path of the image to use and exits 0:
//   - a copied image FILE (Finder ⌘C) is used as it is — its own path is
//     printed and nothing is written, so the original name and encoding
//     survive;
//   - pixels (a screenshot, an image copied from a browser) are written to
//     <out.png> as PNG and that path is printed.
// Exit 1 when the clipboard holds no image, 2 on bad usage.
//
// cosentty embeds this file (src/clipboard.rs, include_str!) and builds
// it once into ~/.cache/cosentty/pbimage-<hash>, the way ime.swift is.
import AppKit

let args = CommandLine.arguments
guard args.count == 2 else {
    FileHandle.standardError.write("usage: pbimage <out.png>\n".data(using: .utf8)!)
    exit(2)
}
let pb = NSPasteboard.general
let imageExts: Set<String> = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tiff"]
if let urls = pb.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL],
   let url = urls.first(where: { imageExts.contains($0.pathExtension.lowercased()) }),
   FileManager.default.fileExists(atPath: url.path) {
    print(url.path)
    exit(0)
}
guard let image = NSImage(pasteboard: pb),
      let tiff = image.tiffRepresentation,
      let rep = NSBitmapImageRep(data: tiff),
      let png = rep.representation(using: .png, properties: [:]) else {
    exit(1)
}
do {
    try png.write(to: URL(fileURLWithPath: args[1]))
    print(args[1])
} catch {
    FileHandle.standardError.write("pbimage: \(error)\n".data(using: .utf8)!)
    exit(1)
}
