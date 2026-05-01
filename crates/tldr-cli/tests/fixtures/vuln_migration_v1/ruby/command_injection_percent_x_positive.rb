require 'net/http'

class DemoController
  def handler(params)
    cmd = params[:cmd]
    %x{#{cmd}}
  end
end
