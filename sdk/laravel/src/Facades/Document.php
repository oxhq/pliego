<?php

declare(strict_types=1);

namespace Pliego\Laravel\Experimental\Facades;

use Illuminate\Support\Facades\Facade;
use Pliego\Laravel\Experimental\DocumentFactory;

/**
 * @method static \Pliego\Laravel\Experimental\PendingDocument view(string $name, array $data = [])
 */
final class Document extends Facade
{
    protected static function getFacadeAccessor(): string
    {
        return DocumentFactory::class;
    }
}
