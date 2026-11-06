// Independent native PDFKit inspection + rasterization, no PDFium calls.
import AppKit
import PDFKit
import Foundation
let args=CommandLine.arguments
precondition(args.count==3,"usage: inspect_pdfkit input.pdf output-directory")
let url=URL(fileURLWithPath:args[1])
let out=URL(fileURLWithPath:args[2],isDirectory:true)
try FileManager.default.createDirectory(at:out,withIntermediateDirectories:true)
guard let doc=PDFDocument(url:url) else { fatalError("PDFKit could not open input") }
var pages:[[String:Any]]=[]
for i in 0..<doc.pageCount {
    let page=doc.page(at:i)!
    let crop=page.bounds(for:.cropBox)
    let swapped=page.rotation==90 || page.rotation==270
    let size=NSSize(width:swapped ? crop.height:crop.width,height:swapped ? crop.width:crop.height)
    let image=page.thumbnail(of:size,for:.cropBox)
    guard let cg=image.cgImage(forProposedRect:nil,context:nil,hints:nil) else {fatalError("PDFKit rasterization failed")}
    let bitmap=NSBitmapImageRep(cgImage:cg)
    try bitmap.representation(using:.png,properties:[:])!.write(to:out.appendingPathComponent("page-\(i).png"))
    var glyphs:[[String:Any]]=[]
    if let text=page.string {
        let ns=text as NSString
        for offset in 0..<ns.length {
            let letter=ns.substring(with:NSRange(location:offset,length:1))
            if letter.trimmingCharacters(in:.whitespacesAndNewlines).isEmpty {continue}
            if let selection=page.selection(for:NSRange(location:offset,length:1)) {
                let b=selection.bounds(for:page)
                glyphs.append(["text":letter,"bounds":[b.minX,b.minY,b.maxX,b.maxY]])
            }
        }
    }
    let annotations=page.annotations.map { a -> [String:Any] in
        let bounds=a.bounds
        let rgb=a.color.usingColorSpace(.deviceRGB)
        let color:[CGFloat]=rgb.map { [$0.redComponent,$0.greenComponent,$0.blueComponent,$0.alphaComponent] } ?? []
        let q=a.quadrilateralPoints?.map { v -> [Double] in
            let p=v.pointValue
            return [p.x+bounds.origin.x,p.y+bounds.origin.y]
        } ?? []
        return ["type":a.type ?? "", "contents":a.contents ?? "", "bounds":[bounds.minX,bounds.minY,bounds.maxX,bounds.maxY],
            "quads_absolute":q,"color":color]
    }
    pages.append(["page_index":i,"label":page.label ?? "","rotation":page.rotation,"crop":[crop.minX,crop.minY,crop.maxX,crop.maxY],
        "text":page.string ?? "","glyphs":glyphs,"annotations":annotations,"width":bitmap.pixelsWide,"height":bitmap.pixelsHigh])
}
let report:[String:Any]=["renderer":"Apple PDFKit", "os":ProcessInfo.processInfo.operatingSystemVersionString,"pages":pages]
try JSONSerialization.data(withJSONObject:report,options:[.prettyPrinted,.sortedKeys]).write(to:out.appendingPathComponent("report.json"))
print("PDFKit independently inspected/rendered \(doc.pageCount) pages: \(out.path)")
