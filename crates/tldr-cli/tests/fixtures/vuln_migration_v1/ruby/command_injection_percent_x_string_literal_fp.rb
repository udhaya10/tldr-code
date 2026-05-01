# vt=CommandInjection lang=ruby — %x{cmd} mention is inside a string literal only.

class DocsOnly
  def docs
    s = "use %x{cmd} for inline shell"
    s
  end
end
